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
