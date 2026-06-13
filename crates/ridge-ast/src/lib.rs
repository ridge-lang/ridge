//! Ridge Abstract Syntax Tree.
//!
//! This crate defines the typed AST produced by `ridge-parser` and consumed by
//! all downstream compiler phases (name resolution, type checking, lowering).
//!
//! # Design principles
//!
//! * Every node carries a [`Span`] — constructing a node without a span is a
//!   compile error at the type level.
//! * [`Span`] is re-exported from `ridge-lexer` so that callers only need to
//!   import one crate for source locations.
//! * The AST is a pure value type: `Clone`, `Debug`, `PartialEq`, `Eq`.  No
//!   reference cycles; no `Arc`/`Rc`.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

// Re-export the canonical source-location type from ridge-lexer so that all
// downstream crates can use `ridge_ast::Span` without also depending on
// `ridge-lexer` directly.
pub use ridge_lexer::Span;

pub mod base;
pub mod block;
pub mod column_mirror;
pub mod decl;
pub mod expr;
pub mod ident;
pub mod lit;
pub mod module;
pub mod pattern;
pub mod ty;
pub mod typeclass;
pub mod visit;

// Flatten the most commonly used types into the crate root for ergonomics.
pub use base::{Capability, DocComment, PrimitiveType, Visibility};
pub use block::Block;
pub use decl::{
    ActorDecl, ActorMember, Attribute, Body, ConstDecl, Constructor, FieldDecl, FnDecl, ImportDecl,
    InitDecl, MailboxConfig, MailboxDecl, MailboxPolicy, ModulePath, OnHandler, Param,
    RecordTypeBody, StateDecl, TypeBody, TypeDecl, UnionTypeBody,
};
pub use expr::{
    AskTimeout, BinOp, Expr, FieldInit, InterpPart, LambdaParam, MatchArm, QualifiedName,
    RecordCtor, UnaryOp,
};
pub use ident::Ident;
pub use lit::Literal;
pub use module::{Item, Module};
pub use pattern::{FieldPattern, ListPatElem, Pattern};
pub use ty::{FnType, RecordTypeField, Type};
pub use typeclass::{ClassConstraint, ClassDecl, FunDep, InstanceDecl, MethodDef, MethodSig};
pub use visit::Visit;

#[cfg(test)]
mod tests {
    use super::*;

    // ── Span (re-export) ──────────────────────────────────────────────────────

    #[test]
    fn span_reexport() {
        let s = Span::point(0);
        assert!(s.is_empty());
    }

    // ── Ident ─────────────────────────────────────────────────────────────────

    #[test]
    fn ident_construction() {
        let span = Span::point(0);
        let id = Ident::new("myVar", span);
        assert_eq!(id.text, "myVar");
        assert_eq!(id.span, span);
    }

    #[test]
    fn ident_is_lower() {
        let span = Span::point(0);
        assert!(Ident::new("foo", span).is_lower());
        assert!(Ident::new("_bar", span).is_lower());
        assert!(!Ident::new("Foo", span).is_lower());
    }

    #[test]
    fn ident_is_upper() {
        let span = Span::point(0);
        assert!(Ident::new("Foo", span).is_upper());
        assert!(!Ident::new("foo", span).is_upper());
    }

    #[test]
    fn ident_is_priv() {
        let span = Span::point(0);
        assert!(Ident::new("_helper", span).is_priv());
        assert!(!Ident::new("_", span).is_priv(), "bare _ is not private");
        assert!(!Ident::new("helper", span).is_priv());
    }

    // ── Literal ───────────────────────────────────────────────────────────────

    #[test]
    fn literal_int_dec() {
        let span = Span::point(0);
        let lit = Literal::IntDec {
            raw: "42".to_string(),
            span,
        };
        assert_eq!(lit.span(), span);
        assert!(matches!(lit, Literal::IntDec { .. }));
    }

    #[test]
    fn literal_int_bin() {
        let span = Span::point(0);
        let lit = Literal::IntBin {
            raw: "0b1010".to_string(),
            span,
        };
        assert_eq!(lit.span(), span);
    }

    #[test]
    fn literal_int_oct() {
        let span = Span::point(0);
        let lit = Literal::IntOct {
            raw: "0o17".to_string(),
            span,
        };
        assert_eq!(lit.span(), span);
    }

    #[test]
    fn literal_int_hex() {
        let span = Span::point(0);
        let lit = Literal::IntHex {
            raw: "0xFF".to_string(),
            span,
        };
        assert_eq!(lit.span(), span);
    }

    #[test]
    fn literal_float() {
        let span = Span::point(0);
        let lit = Literal::Float {
            raw: "3.14".to_string(),
            span,
        };
        assert_eq!(lit.span(), span);
    }

    #[test]
    fn literal_bool() {
        let span = Span::point(0);
        let lit_true = Literal::Bool { value: true, span };
        let lit_false = Literal::Bool { value: false, span };
        assert_eq!(lit_true.span(), span);
        assert_eq!(lit_false.span(), span);
    }

    #[test]
    fn literal_text() {
        let span = Span::point(0);
        let lit = Literal::Text {
            raw: r#""hello""#.to_string(),
            span,
        };
        assert_eq!(lit.span(), span);
    }

    // ── Visibility ────────────────────────────────────────────────────────────

    #[test]
    fn visibility_variants() {
        assert_eq!(Visibility::Private, Visibility::Private);
        assert_eq!(Visibility::Pub, Visibility::Pub);
        assert_eq!(Visibility::PubInternal, Visibility::PubInternal);
        assert_eq!(Visibility::default(), Visibility::Private);
    }

    // ── Capability ────────────────────────────────────────────────────────────

    #[test]
    fn capability_variants() {
        let caps = [
            Capability::Io,
            Capability::Fs,
            Capability::Net,
            Capability::Time,
            Capability::Random,
            Capability::Env,
            Capability::Proc,
            Capability::Spawn,
            Capability::Ffi,
            Capability::Db,
        ];
        assert_eq!(caps.len(), 10);
    }

    // ── PrimitiveType ─────────────────────────────────────────────────────────

    #[test]
    fn primitive_type_variants() {
        let prims = [
            PrimitiveType::Int,
            PrimitiveType::Float,
            PrimitiveType::Bool,
            PrimitiveType::Text,
            PrimitiveType::Unit,
            PrimitiveType::Timestamp,
        ];
        assert_eq!(prims.len(), 6);
    }

    // ── DocComment ────────────────────────────────────────────────────────────

    #[test]
    fn doc_comment_construction() {
        let span = Span::new(0, 20);
        let doc = DocComment {
            text: "This is a doc comment.".to_string(),
            span,
        };
        assert_eq!(doc.text, "This is a doc comment.");
        assert_eq!(doc.span, span);
    }

    // ── Module ────────────────────────────────────────────────────────────────

    #[test]
    fn module_empty_construction() {
        let span = Span::point(0);
        let module = Module {
            items: vec![],
            doc: vec![],
            span,
        };
        assert!(module.items.is_empty());
        assert!(module.doc.is_empty());
        assert_eq!(module.span, span);
    }
}
