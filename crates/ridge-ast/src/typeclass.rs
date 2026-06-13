//! Typeclass-related AST nodes (`class`, `instance`, `where`-constraints).
//!
//! Introduced in 0.2.13. Downstream phases (resolve, typecheck, lower) treat
//! these as deferred items for this release cycle — they are parsed and stored
//! in the AST but no semantic pass runs against them yet.

use crate::{decl::Param, DocComment, Expr, Ident, Span, Type};

// ── ClassConstraint ───────────────────────────────────────────────────────────

/// A class constraint in a `where` clause or superclass list.
///
/// Written as `ClassName tyVar…`, e.g. `ToText a`, `Eq b`, or — for a
/// multi-parameter class — `Convert a b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassConstraint {
    /// The class name (`UPPER_IDENT`), e.g. `ToText`, `Eq`, `Ord`.
    pub class: Ident,
    /// The constrained type variables (`LOWER_IDENT`), e.g. `a` — or `a b`
    /// for a multi-parameter class. Always at least one.
    pub ty_vars: Vec<Ident>,
    /// Span covering `ClassName tyVar…`.
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

// ── FunDep ────────────────────────────────────────────────────────────────────

/// A functional dependency on a class, written `from… -> to…`.
///
/// In `class Refinable q p | q -> p`, the dependency `q -> p` declares that the
/// `from` variables (`q`) uniquely determine the `to` variables (`p`): two
/// instances that agree on the `from` positions must agree on the `to` ones, and
/// knowing the `from` types lets the solver infer the `to` types without an
/// annotation. The variables are drawn from the class's own `ty_vars`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FunDep {
    /// The determining type variables (left of `->`). Always at least one.
    pub from: Vec<Ident>,
    /// The determined type variables (right of `->`). Always at least one.
    pub to: Vec<Ident>,
    /// Span covering `from… -> to…`.
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
///
/// class Convert a b =
///     convert (x: a) -> b
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassDecl {
    /// The class name (`UPPER_IDENT`).
    ///
    /// `Show` is desugared to `ToText` at parse time; this field always holds
    /// the canonical name.
    pub name: Ident,
    /// The class's type variables (`LOWER_IDENT`). One for an ordinary class,
    /// several for a multi-parameter class. Always at least one.
    pub ty_vars: Vec<Ident>,
    /// Functional dependencies (`| q -> p, …`), empty when none are written.
    /// Each names variables drawn from [`ClassDecl::ty_vars`]; the determining
    /// set on the left of `->` fixes the determined set on the right.
    pub fundeps: Vec<FunDep>,
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
///
/// instance Encode (List a) where Encode a =
///     encode (xs) = ...
///
/// instance Convert Celsius Fahrenheit =
///     convert (c) = ...
/// ```
///
/// Parametric instances carry a `where` clause listing the constraints on
/// the head's type variable(s), e.g. `where Encode a` for `Encode (List a)`.
/// Non-parametric instances have an empty `constraints` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstanceDecl {
    /// The class being instantiated (`UPPER_IDENT`).
    ///
    /// `Show` is desugared to `ToText` at parse time.
    pub class: Ident,
    /// The concrete head types the instance applies to — one type atom per
    /// class parameter. One entry for an ordinary class (`Encode (List a)`),
    /// several for a multi-parameter class (`Convert Celsius Fahrenheit`).
    /// Always at least one.
    pub head: Vec<Type>,
    /// Constraints on the instance head's type variable(s) (`where C a`).
    ///
    /// Non-empty only for parametric instances such as
    /// `instance Encode (List a) where Encode a`. The collect pass populates
    /// [`InstanceInfo::ctx_constraints`] from this field.
    pub constraints: Vec<ClassConstraint>,
    /// Method definitions (at least one required).
    pub methods: Vec<MethodDef>,
    /// Span covering the full declaration.
    pub span: Span,
    /// Attached doc comment.
    pub doc: Option<DocComment>,
}
