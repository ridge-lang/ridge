//! Top-level module node and the `Item` enum.

use crate::{
    decl::{ActorDecl, ConstDecl, FnDecl, ImportDecl, TypeDecl},
    typeclass::{ClassDecl, InstanceDecl},
    DocComment, Span,
};

/// A parsed Ridge source file.
///
/// The parser always produces a `Module`, even if the source is empty or
/// contains errors (partial AST in error-recovery mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// Top-level declarations in source order.
    pub items: Vec<Item>,
    /// File-level doc comments that precede the first declaration.
    pub doc: Vec<DocComment>,
    /// Span covering the entire source file.
    pub span: Span,
}

/// A top-level declaration in a Ridge module (grammar §2.1 line 303).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// An `import` declaration.
    Import(ImportDecl),
    /// A `const` declaration.
    Const(ConstDecl),
    /// A `type` declaration.
    Type(TypeDecl),
    /// A `fn` declaration.
    Fn(FnDecl),
    /// An `actor` declaration.
    Actor(ActorDecl),
    /// A `class` declaration (typeclass definition).
    ///
    /// Parsed from 0.2.13 onwards. Semantic passes (resolve, typecheck,
    /// lower) handle these items in later cuts.
    ClassDecl(ClassDecl),
    /// An `instance` declaration (typeclass instance).
    ///
    /// Parsed from 0.2.13 onwards. Semantic passes handle these in later cuts.
    InstanceDecl(InstanceDecl),
}

impl Item {
    /// The item's full source extent, first byte to last.
    ///
    /// This starts at whatever the source writes above the declaration and
    /// treats as part of it — a doc comment, a `@test` attribute — not at the
    /// `fn` / `type` / `const` keyword.  A declaration and the item that holds
    /// it are different things, and each decl's own `span` field covers the
    /// narrower one: that is what a diagnostic points at, and widening it
    /// would move error carets onto doc comments.
    ///
    /// Anything reasoning about where an item sits on the page wants this one.
    /// A formatter that asks the declaration instead reads the attribute as
    /// belonging to the gap between two items and separates it from the
    /// function it annotates; folding ranges built the same way leave it
    /// outside the fold.
    #[must_use]
    pub fn span(&self) -> Span {
        match self {
            Self::Import(d) => span_with_doc(d.span, d.doc.as_ref()),
            Self::Const(d) => span_with_doc(d.span, d.doc.as_ref()),
            Self::Type(d) => span_with_doc(d.span, d.doc.as_ref()),
            Self::Fn(d) => d
                .attrs
                .iter()
                .fold(span_with_doc(d.span, d.doc.as_ref()), |acc, attr| {
                    acc.merge(attr.span())
                }),
            Self::Actor(d) => span_with_doc(d.span, d.doc.as_ref()),
            Self::ClassDecl(d) => span_with_doc(d.span, d.doc.as_ref()),
            Self::InstanceDecl(d) => span_with_doc(d.span, d.doc.as_ref()),
        }
    }
}

/// Extend a declaration's span backwards over its doc comment, if it has one.
fn span_with_doc(span: Span, doc: Option<&DocComment>) -> Span {
    doc.map_or(span, |d| span.merge(d.span))
}
