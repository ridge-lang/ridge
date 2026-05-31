//! Typeclass-related AST nodes (`class`, `instance`, `where`-constraints).
//!
//! Introduced in 0.2.13. Downstream phases (resolve, typecheck, lower) treat
//! these as deferred items for this release cycle — they are parsed and stored
//! in the AST but no semantic pass runs against them yet.

use crate::{decl::Param, DocComment, Expr, Ident, Span, Type};

// ── ClassConstraint ───────────────────────────────────────────────────────────

/// A single class constraint in a `where` clause or superclass list.
///
/// Written as `ClassName tyVar`, e.g. `ToText a` or `Eq b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassConstraint {
    /// The class name (`UPPER_IDENT`), e.g. `ToText`, `Eq`, `Ord`.
    pub class: Ident,
    /// The constrained type variable (`LOWER_IDENT`), e.g. `a`.
    pub ty_var: Ident,
    /// Span covering `ClassName tyVar`.
    pub span: Span,
}

// ── MethodSig ─────────────────────────────────────────────────────────────────

/// A bare method signature inside a `class` declaration body.
///
/// Written as `name (params) -> RetType` with NO `fn` keyword and NO body.
/// Example: `toText (x: a) -> Text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodSig {
    /// Method name (`LOWER_IDENT`).
    pub name: Ident,
    /// Method parameters.
    pub params: Vec<Param>,
    /// Return type.
    pub ret: Type,
    /// Span covering the full signature.
    pub span: Span,
}

// ── MethodDef ─────────────────────────────────────────────────────────────────

/// A method definition inside an `instance` declaration body.
///
/// Written as `name (params) -> RetType = body` with NO `fn` keyword.
/// Example: `toText (c: Color) -> Text = "red"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodDef {
    /// Method name (`LOWER_IDENT`).
    pub name: Ident,
    /// Method parameters.
    pub params: Vec<Param>,
    /// Return type.
    pub ret: Type,
    /// Method body expression.
    pub body: Expr,
    /// Span covering the full definition.
    pub span: Span,
}

// ── ClassDecl ─────────────────────────────────────────────────────────────────

/// A `class` declaration.
///
/// ```text
/// class Show a =
///     toText (x: a) -> Text
///
/// class Ord a where Eq a =
///     compare (x: a) (y: a) -> Ordering
/// ```
///
/// In 0.2.13 only single-parameter classes are supported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDecl {
    /// The class name (`UPPER_IDENT`).
    ///
    /// `Show` is desugared to `ToText` at parse time; this field always holds
    /// the canonical name.
    pub name: Ident,
    /// The class's single type variable (`LOWER_IDENT`).
    pub ty_var: Ident,
    /// Optional superclass constraints (`where C a`).
    pub superclasses: Vec<ClassConstraint>,
    /// Method signatures (at least one required).
    pub methods: Vec<MethodSig>,
    /// Span covering the full declaration.
    pub span: Span,
    /// Attached doc comment.
    pub doc: Option<DocComment>,
}

// ── InstanceDecl ─────────────────────────────────────────────────────────────

/// An `instance` declaration.
///
/// ```text
/// instance Show Color =
///     toText (c: Color) -> Text = match c
///         Red   => "red"
///         Green => "green"
///         Blue  => "blue"
/// ```
///
/// In 0.2.13 instance heads cannot carry `where` constraints.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceDecl {
    /// The class being instantiated (`UPPER_IDENT`).
    ///
    /// `Show` is desugared to `ToText` at parse time.
    pub class: Ident,
    /// The concrete type the instance applies to.
    pub ty: Type,
    /// Method definitions (at least one required).
    pub methods: Vec<MethodDef>,
    /// Span covering the full declaration.
    pub span: Span,
    /// Attached doc comment.
    pub doc: Option<DocComment>,
}
