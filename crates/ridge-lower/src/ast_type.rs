//! AST `Type` → `ridge_types::Type` adapter — private helper for Phase 5.
//!
//! Provides a mechanical, 1:1 mapping from the syntactic [`ridge_ast::Type`]
//! to the semantic [`ridge_types::Type`].  This mapping is intentionally
//! shallow: it resolves named types by looking up `TyConId`s in the workspace
//! arena, but does NOT run type inference, unification, or substitution.
//!
//! # What this module resolves
//!
//! | AST variant | Result |
//! |---|---|
//! | `Type::Primitive` | `Type::Con(builtin_id, [])` |
//! | `Type::Named { name }` | `Type::Con(lookup(name), [])` or `Type::Error` |
//! | `Type::App { head, args }` | `Type::Con(lookup(head), lowered_args)` |
//! | `Type::Tuple { elems }` | `Type::Tuple(elems.map(lower_ast_type))` |
//! | `Type::List { elem }` | `Type::Con(list_id, [lower_ast_type(elem)])` |
//! | `Type::Fn { fn_ty }` | `Type::Fn { params, ret, caps }` |
//! | `Type::Paren { inner }` | delegates to `lower_ast_type(inner)` |
//! | `Type::Var` | `Type::Error` — unification vars unresolved at lowering |
//!
//! # `Type::Error` fallback
//!
//! Any variant that cannot be mapped (e.g. a `Type::Var`, or a `Named` whose
//! name is absent from the workspace) falls back to `Type::Error`.  This is
//! correct: `Type::Error` is the absorbing sentinel for the Phase 5 IR and
//! prevents cascading L### diagnostics.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use ridge_ast::{Capability, PrimitiveType};
use ridge_resolve::NodeKind;
use ridge_types::{CapRow, CapabilitySet, Type};

use crate::ctx::LowerCtx;

/// Lower a syntactic [`ridge_ast::Type`] to a semantic [`ridge_types::Type`].
///
/// The conversion is mechanical and shallow: it looks up `TyConId`s by name
/// from the workspace arena, but does not run inference or substitution.  Any
/// variant that cannot be resolved falls back to [`Type::Error`].
///
/// # When the result is `Type::Error`
///
/// - `Type::Var` — unification variables are unresolved at lowering time.
/// - `Type::Named` / `Type::App` whose name is not in the workspace tycon list
///   (the workspace is absent in unit tests, or the name is misspelled).
/// - Any future AST `Type` variant not handled here.
#[must_use]
pub(crate) fn lower_ast_type(ctx: &mut LowerCtx<'_>, ast_ty: &ridge_ast::Type) -> Type {
    match ast_ty {
        // ── Primitive scalars — map directly to builtin TyConIds ─────────────
        ridge_ast::Type::Primitive { name, .. } => lower_primitive(ctx, *name),

        // ── Named type constructor with no arguments ─────────────────────────
        ridge_ast::Type::Named { name, .. } => ctx
            .lookup_tycon_by_name(&name.text)
            .map_or(Type::Error, |id| Type::Con(id, vec![])),

        // ── Type constructor applied to one or more arguments ────────────────
        ridge_ast::Type::App { head, args, .. } => {
            let Some(id) = ctx.lookup_tycon_by_name(&head.text) else {
                return Type::Error;
            };
            let lowered_args: Vec<Type> = args.iter().map(|a| lower_ast_type(ctx, a)).collect();
            Type::Con(id, lowered_args)
        }

        // ── Tuple type ───────────────────────────────────────────────────────
        ridge_ast::Type::Tuple { elems, .. } => {
            let lowered: Vec<Type> = elems.iter().map(|e| lower_ast_type(ctx, e)).collect();
            Type::Tuple(lowered)
        }

        // ── List sugar `[a]` → `Con(list_id, [lower(a)])` ───────────────────
        ridge_ast::Type::List { elem, .. } => {
            let elem_ty = lower_ast_type(ctx, elem);
            // Look up the List TyCon (builtin name "List", always present once
            // the workspace is wired).
            ctx.lookup_tycon_by_name("List")
                .map_or(Type::Error, |list_id| Type::Con(list_id, vec![elem_ty]))
        }

        // ── Function type ────────────────────────────────────────────────────
        ridge_ast::Type::Fn { fn_ty, .. } => {
            let params: Vec<Type> = fn_ty
                .params
                .iter()
                .map(|p| lower_ast_type(ctx, p))
                .collect();
            let ret = Box::new(lower_ast_type(ctx, &fn_ty.ret));
            let caps = caps_from_ast_caps(&fn_ty.caps);
            Type::Fn { params, ret, caps }
        }

        // ── Paren erasure ────────────────────────────────────────────────────
        ridge_ast::Type::Paren { inner, .. } => lower_ast_type(ctx, inner),

        // ── Type variables are unresolved at lowering time ───────────────────
        //
        // Look up the resolved type via
        // `node_id_map.get(type_span, NodeKind::Type)`. Falls back to
        // `Type::Error` when no node_id_map is attached or the var has no entry.
        ridge_ast::Type::Var { span, .. } => ctx
            .node_id_map
            .as_ref()
            .and_then(|m| m.get(*span, NodeKind::Type))
            .and_then(|nid| ctx.node_type(nid).cloned())
            .unwrap_or(Type::Error),
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Map a [`PrimitiveType`] to its `Type::Con(builtin_id, [])` form.
///
/// Looks up the canonical builtin name in the workspace arena so that the
/// returned `TyConId` matches the one Phase 4 used.
fn lower_primitive(ctx: &mut LowerCtx<'_>, prim: PrimitiveType) -> Type {
    let name = primitive_name(prim);
    ctx.lookup_tycon_by_name(name)
        .map_or(Type::Error, |id| Type::Con(id, vec![]))
}

/// Return the canonical builtin name string for a [`PrimitiveType`].
const fn primitive_name(prim: PrimitiveType) -> &'static str {
    match prim {
        PrimitiveType::Int => "Int",
        PrimitiveType::Float => "Float",
        PrimitiveType::Bool => "Bool",
        PrimitiveType::Text => "Text",
        PrimitiveType::Unit => "Unit",
        PrimitiveType::Timestamp => "Timestamp",
    }
}

/// Convert a slice of AST [`Capability`] values to a [`CapRow::Concrete`].
///
/// Replicates the logic from `ridge-typecheck::caps_check::caps_from_ast_slice`
/// locally to avoid a dependency on the typecheck crate's internal module
/// from our adapter.  The two implementations must stay in sync; both simply
/// fold the slice into a `CapabilitySet` via `CapabilitySet::insert`.
fn caps_from_ast_caps(caps: &[Capability]) -> CapRow {
    let mut cs = CapabilitySet::PURE;
    for &c in caps {
        cs.insert(c);
    }
    CapRow::Concrete(cs)
}
