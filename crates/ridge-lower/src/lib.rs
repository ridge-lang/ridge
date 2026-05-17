//! Ridge Phase 5 lowering engine.
//!
//! Consumes a [`TypedWorkspace`] from `ridge-typecheck` and produces a
//! target-neutral [`LoweredWorkspace`] (Ridge Core IR). All Phase-5 desugar
//! rules live in per-rule modules under
//! this crate; T2 ships only the engine scaffold and [`LowerCtx`].
//!
//! # Entry points
//!
//! - [`lower_workspace`] — lower an entire [`TypedWorkspace`] to Core IR.
//! - [`lower_module`] — lower a single [`TypedModule`] (LSP hot-path; T3+).

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

pub mod actor_lower;
pub mod ast_type;
pub mod block;
pub mod core;
pub mod ctx;
pub mod error;
pub mod field_accessor;
pub mod guard;
pub mod if_lower;
pub mod inner_fn;
pub mod interp;
pub mod item;
pub mod match_lower;
pub mod operators;
pub mod pipe;
pub mod propagate;
pub mod try_block;
pub mod with_update;

pub use ctx::LowerCtx;
pub use error::LowerError;

use ridge_ir::{LoweredModule, LoweredWorkspace};
use ridge_resolve::{assign_node_ids, ResolvedWorkspace};
use ridge_typecheck::{TypedModule, TypedWorkspace};

/// Lower an entire typed workspace to Core IR.
///
/// Iterates over every module in `twork` and calls [`lower_module`]
/// for each one, collecting the results into a [`LoweredWorkspace`] whose slot
/// count equals `twork.modules.len()`.  Every slot is `Some(...)` — skipping
/// error-producing modules is a T3+ concern once the per-module error handling
/// policy is wired.
///
/// `rwork` is the resolved workspace produced by Phase 3; it carries the
/// per-module [`ridge_resolve::BindingMap`] tables needed by the `Ident` /
/// `Qualified` lowering rules (Option A — T3).
///
/// On an empty workspace (`twork.modules` is empty and `twork.tycons` is empty)
/// this returns an empty [`LoweredWorkspace`] with `tycon_count == 0`, satisfying
/// the definition-of-done literal.
#[must_use]
pub fn lower_workspace(twork: &TypedWorkspace, rwork: &ResolvedWorkspace) -> LoweredWorkspace {
    let modules = twork
        .modules
        .iter()
        .enumerate()
        .map(|(i, typed)| {
            let rmod = rwork.modules.get(i);
            Some(lower_module(typed, twork, rmod))
        })
        .collect();
    // Safety: a workspace with more than 2^32 TyCons is not a valid Ridge
    // program; treat overflow as saturating (defensive).
    let tycon_count = u32::try_from(twork.tycons.len()).unwrap_or(u32::MAX);
    LoweredWorkspace::new(modules, tycon_count)
}

/// Lower a single typed module to Core IR.
///
/// Constructs a fresh [`LowerCtx`], attaches the resolve-layer binding tables
/// (Option A — T3), walks all top-level [`ridge_ast::Item`]s via
/// [`item::lower_item`], and calls [`LowerCtx::finish_with_items`] to produce a
/// [`LoweredModule`] whose `items` vector mirrors source order and whose
/// `node_types` length matches `typed.node_types.len()` (index-parity invariant).
///
/// `ws` carries workspace-level context (tycons, builtins) for `with` schema
/// lookup (§4.5) and interp `ToText` dispatch (§4.6).
///
/// `rmod` is the `ResolvedModule` for this module; when `Some`, the binding
/// side-tables are attached to the context so that `Ident`/`Qualified` atoms
/// resolve correctly (§3.2).  `None` is accepted defensively for test scaffolding
/// that does not run the full resolve pipeline.
#[must_use]
pub fn lower_module(
    typed: &TypedModule,
    ws: &TypedWorkspace,
    rmod: Option<&ridge_resolve::ResolvedModule>,
) -> LoweredModule {
    let mut ctx = LowerCtx::new(typed.id, &typed.node_types);
    // Attach workspace-level context (tycons + builtins) for `with` schema
    // lookup (§4.5) and interp `ToText` dispatch (§4.6).
    ctx.attach_workspace(ws);
    // Attach the current module's inferred_caps side-table so that
    // lookup_inferred_caps can read Phase 4's capability inference results.
    ctx.attach_inferred_caps(&typed.inferred_caps);
    if let Some(rm) = rmod {
        // Reconstruct the NodeIdMap from the module AST so that
        // (Span, NodeKind) → NodeId lookups are available during lowering.
        // assign_node_ids is cheap (single AST traversal) and avoids the need
        // to store NodeIdMap persistently in ResolvedModule.
        let (nid_map, _nid_errors) = assign_node_ids(&typed.ast);
        ctx.attach_bindings(nid_map, &rm.bindings);
        // Wire the per-module symbol table for SymbolId → owner-type-name
        // lookup used by lookup_constructor_tycon (§3.2 / OQ-PHASE45-007).
        // ResolvedModule.symbols is the SymbolTable built by the T6 collector.
        ctx.attach_symbol_table(&rm.symbols);
    }

    // ── Item walker — lower each top-level item in source order ──────────────
    let items: Vec<ridge_ir::IrItem> = typed
        .ast
        .items
        .iter()
        .filter_map(|ast_item| item::lower_item(&mut ctx, ast_item))
        .collect();

    ctx.finish_with_items(items)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ir::{IrNodeId, LoweredWorkspace, ModuleId, NodeId};

    // ── Test 1: lower_workspace on an empty workspace returns empty output ─────
    //
    // TypedWorkspace is #[non_exhaustive] and lives in ridge-typecheck, so we
    // cannot construct it here directly.  Instead we verify the structural
    // contract via the constructors we added on ridge-ir.
    //
    // T3 will add an integration-style smoke test that runs the full
    // resolve → typecheck → lower_workspace pipeline on a trivial Ridge source.
    #[test]
    fn lowered_workspace_empty_constructor() {
        let ws = LoweredWorkspace::empty(0, 0);
        assert!(ws.modules.is_empty(), "expected no modules");
        assert_eq!(ws.tycon_count, 0, "expected tycon_count 0");
    }

    // ── Test 2: LowerCtx fresh_id density and provenance ─────────────────────
    #[test]
    fn lower_ctx_fresh_id_density_and_provenance() {
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);

        let id0 = ctx.fresh_id(Some(NodeId(0)));
        let id1 = ctx.fresh_id(None);
        let id2 = ctx.fresh_id(Some(NodeId(2)));

        // IDs must be dense starting at 0.
        assert_eq!(id0, IrNodeId(0));
        assert_eq!(id1, IrNodeId(1));
        assert_eq!(id2, IrNodeId(2));

        // source_map must contain exactly the two entries with Some(origin).
        // The synthetic node (id1, None origin) must NOT appear.
        assert_eq!(
            ctx.source_map.len(),
            2,
            "expected exactly 2 provenance entries"
        );
        assert_eq!(ctx.source_map.get(&IrNodeId(0)), Some(&NodeId(0)));
        assert_eq!(
            ctx.source_map.get(&IrNodeId(1)),
            None,
            "synthetic node must not appear in source_map"
        );
        assert_eq!(ctx.source_map.get(&IrNodeId(2)), Some(&NodeId(2)));
    }

    // ── Test 3: LowerCtx fresh_local unique names ─────────────────────────────
    #[test]
    fn lower_ctx_fresh_local_unique() {
        let mut ctx = LowerCtx::new(ModuleId(0), &[]);

        let name0 = ctx.fresh_local("__prop_ok");
        let name1 = ctx.fresh_local("__prop_ok");
        let name2 = ctx.fresh_local("__with_base");

        // Counter is shared across prefixes — each call increments once.
        assert_eq!(name0, "__prop_ok_0");
        assert_eq!(name1, "__prop_ok_1");
        assert_eq!(name2, "__with_base_2");
    }

    // ── Test 4: LowerCtx propagation scope stack ──────────────────────────────
    #[test]
    fn lower_ctx_propagation_scope_stack() {
        use ridge_types::Type;

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);

        // Initially empty.
        assert!(ctx.current_propagation_scope().is_none());

        // Push one scope.
        ctx.push_propagation_scope(Type::Error);
        assert!(ctx.current_propagation_scope().is_some());

        // Pop returns it.
        let popped = ctx.pop_propagation_scope();
        assert!(popped.is_some());

        // Now empty again; pop returns None.
        let empty = ctx.pop_propagation_scope();
        assert!(empty.is_none(), "pop on empty stack must return None");
    }
}
