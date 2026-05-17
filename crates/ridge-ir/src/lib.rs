//! Ridge Core IR — pure data crate.
//!
//! Defines all IR types produced by `ridge-lower` (Phase 5) and consumed by
//! Phase 6 codegen backends, stdlib lowering, and LSP.
//!
//! This crate has **no transformation logic** — it is a leaf data crate with
//! zero I/O and zero diagnostic emission.  All types are `#[non_exhaustive]`
//! where the plan mandates it; every `pub` item carries rustdoc.
//!
//! # Quick-start
//!
//! ```rust
//! use ridge_ir::{IrNodeId, IrExpr, IrLit, LoweredModule, LoweredWorkspace};
//! ```

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

pub mod actor;
pub mod expr;
pub mod id;
pub mod item;
pub mod lit;
pub mod pat;
pub mod symbol;
pub mod workspace;

// ── Flat re-exports so consumers write `ridge_ir::IrExpr`, not `ridge_ir::expr::IrExpr` ──

pub use actor::{IrActor, IrHandler, IrInit, IrStateField};
pub use expr::{AssignTarget, IrArm, IrExpr, IrTimeout};
pub use id::IrNodeId;
pub use item::{IrConst, IrFfiFn, IrFn, IrItem, IrParam};
pub use lit::IrLit;
pub use pat::IrPat;
pub use symbol::{CtorKind, SymbolRef};
pub use workspace::{LoweredModule, LoweredWorkspace};

// Re-export upstream types so consumers of `ridge-ir` can avoid additional deps
// for the types that ride along on IR nodes.
pub use ridge_ast::Span;
pub use ridge_resolve::{ModuleId, NodeId};
pub use ridge_types::{CapabilitySet, Scheme, TyConId, Type};

#[cfg(test)]
mod tests {
    use super::*;
    use rustc_hash::FxHashMap;

    // ── Helper: minimal Span ────────────────────────────────────────────────

    fn span() -> Span {
        Span::point(0)
    }

    // ── Test 1: IrNodeId equality and hash sanity ───────────────────────────

    #[test]
    fn ir_node_id_eq_hash() {
        use std::collections::HashSet;
        let a = IrNodeId(0);
        let b = IrNodeId(0);
        let c = IrNodeId(1);
        assert_eq!(a, b);
        assert_ne!(a, c);
        let mut set = HashSet::new();
        set.insert(a);
        set.insert(b); // same as a
        set.insert(c);
        assert_eq!(set.len(), 2);
    }

    // ── Test 2: IrNodeId density — usable as Vec index ─────────────────────

    #[test]
    fn ir_node_id_density_vec_index() {
        let mut node_types: Vec<Option<&str>> = Vec::new();
        for i in 0u32..8 {
            node_types.push(Some("Type"));
            assert!(node_types.get(IrNodeId(i).0 as usize).is_some());
        }
        assert_eq!(node_types.len(), 8);
    }

    // ── Test 3: #[non_exhaustive] — all required types constructible inside the crate ──

    #[test]
    fn non_exhaustive_lowered_workspace_constructible() {
        // `LoweredWorkspace` is #[non_exhaustive]; we can construct it here (inside crate).
        let ws = LoweredWorkspace {
            modules: vec![],
            tycon_count: 0,
        };
        assert!(ws.modules.is_empty());
    }

    #[test]
    fn non_exhaustive_lowered_module_constructible() {
        let m = LoweredModule {
            id: ModuleId(0),
            items: vec![],
            node_types: vec![],
            source_map: FxHashMap::default(),
        };
        assert!(m.items.is_empty());
    }

    #[test]
    fn non_exhaustive_ir_item_all_variants() {
        // Exercise IrItem variants — ensures the enum variants compile.
        // Use Type::Error as the placeholder type (the absorbing error type
        // is valid for pure data tests that do not run typecheck).
        let lit_expr = IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Unit,
            span: span(),
        };
        let item_fn = IrItem::Fn(IrFn {
            name: "f".into(),
            module: ModuleId(0),
            params: vec![],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body: lit_expr,
            origin: NodeId(0),
            span: span(),
            is_pub: false,
            is_main: false,
            doc: None,
        });
        assert!(matches!(item_fn, IrItem::Fn(_)));
    }

    // ── Test 4: IrExpr atom variants ────────────────────────────────────────

    #[test]
    fn ir_expr_atom_variants() {
        let lit = IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Int(42),
            span: span(),
        };
        assert!(matches!(lit, IrExpr::Lit { .. }));

        let local = IrExpr::Local {
            id: IrNodeId(1),
            name: "x".into(),
            span: span(),
        };
        assert!(matches!(local, IrExpr::Local { .. }));

        let sym = IrExpr::Symbol {
            id: IrNodeId(2),
            sym: SymbolRef::Prelude {
                name: "None".into(),
            },
            span: span(),
        };
        assert!(matches!(sym, IrExpr::Symbol { .. }));
    }

    // ── Test 5: IrExpr call and construct variants ───────────────────────────

    #[test]
    fn ir_expr_call_and_construct() {
        let callee = IrExpr::Symbol {
            id: IrNodeId(0),
            sym: SymbolRef::Prelude {
                name: "Some".into(),
            },
            span: span(),
        };
        let arg = IrExpr::Lit {
            id: IrNodeId(1),
            value: IrLit::Int(1),
            span: span(),
        };
        let call = IrExpr::Call {
            id: IrNodeId(2),
            callee: Box::new(callee),
            args: vec![arg],
            span: span(),
        };
        assert!(matches!(call, IrExpr::Call { .. }));

        let construct = IrExpr::Construct {
            id: IrNodeId(3),
            ctor: SymbolRef::Prelude {
                name: "None".into(),
            },
            fields: vec![],
            span: span(),
        };
        assert!(matches!(construct, IrExpr::Construct { .. }));
    }

    // ── Test 6: IrExpr match and block variants ──────────────────────────────

    #[test]
    fn ir_expr_match_and_block() {
        let scrutinee = IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Bool(true),
            span: span(),
        };
        let arm_body = IrExpr::Lit {
            id: IrNodeId(1),
            value: IrLit::Unit,
            span: span(),
        };
        let arm = IrArm {
            pat: IrPat::Wild { span: span() },
            when: None,
            body: arm_body,
            span: span(),
        };
        let m = IrExpr::Match {
            id: IrNodeId(2),
            scrutinee: Box::new(scrutinee),
            arms: vec![arm],
            span: span(),
        };
        assert!(matches!(m, IrExpr::Match { .. }));

        let stmt1 = IrExpr::Lit {
            id: IrNodeId(3),
            value: IrLit::Unit,
            span: span(),
        };
        let block = IrExpr::Block {
            id: IrNodeId(4),
            stmts: vec![stmt1],
            span: span(),
        };
        assert!(matches!(block, IrExpr::Block { .. }));
    }

    // ── Test 7: Tiny LoweredModule with one IrFn ────────────────────────────

    #[test]
    fn lowered_module_with_one_fn() {
        let body = IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Unit,
            span: span(),
        };
        let f = IrFn {
            name: "hello".into(),
            module: ModuleId(0),
            params: vec![],
            ret_ty: Type::Error,
            caps: CapabilitySet::PURE,
            scheme: Scheme::mono(Type::Error),
            body,
            origin: NodeId(0),
            span: span(),
            is_pub: true,
            is_main: false,
            doc: None,
        };
        let mut node_types = vec![None; 1];
        node_types[0] = Some(Type::Error);
        let m = LoweredModule {
            id: ModuleId(0),
            items: vec![IrItem::Fn(f)],
            node_types,
            source_map: FxHashMap::default(),
        };
        assert_eq!(m.items.len(), 1);
        assert!(matches!(m.items[0], IrItem::Fn(_)));
        assert!(m.node_types[IrNodeId(0).0 as usize].is_some());
    }

    // ── Test 8: IrLit and IrPat non-exhaustive variants ─────────────────────

    #[test]
    fn ir_lit_variants() {
        assert!(matches!(IrLit::Int(0), IrLit::Int(0)));
        assert!(matches!(IrLit::Float(1.0), IrLit::Float(_)));
        assert!(matches!(IrLit::Bool(true), IrLit::Bool(true)));
        assert!(matches!(IrLit::Text("hi".into()), IrLit::Text(_)));
        assert!(matches!(IrLit::Unit, IrLit::Unit));
        assert!(matches!(IrLit::EmptyList, IrLit::EmptyList));
    }

    #[test]
    fn ir_pat_variants() {
        let wild = IrPat::Wild { span: span() };
        assert!(matches!(wild, IrPat::Wild { .. }));

        let lit_pat = IrPat::Lit {
            value: IrLit::Int(0),
            span: span(),
        };
        assert!(matches!(lit_pat, IrPat::Lit { .. }));

        let bind = IrPat::Bind {
            name: "x".into(),
            inner: None,
            span: span(),
        };
        assert!(matches!(bind, IrPat::Bind { .. }));
    }
}
