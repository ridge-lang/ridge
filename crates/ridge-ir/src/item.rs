//! Top-level IR items: functions, constants, and actors (actors are in `actor.rs`).
// OQ-IR003: IrItem is #[non_exhaustive] — see expr.rs for rationale.

use crate::actor::IrActor;
use crate::expr::IrExpr;
use ridge_ast::Span;
use ridge_resolve::{ModuleId, NodeId};
use ridge_types::{CapabilitySet, Scheme, Type};

/// A top-level item in the Ridge Core IR.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum IrItem {
    /// A top-level function declaration.
    Fn(IrFn),
    /// An actor declaration.
    Actor(IrActor),
    /// A top-level constant declaration.
    Const(IrConst),
    // `Item::Type` and `Item::Import` are erased at this stage — type
    // declarations are looked up via `TypedWorkspace.tycons` and imports
    // have been fully resolved into the per-NodeId BindingMap.
    // (No variant for them.)
    /// An `@ffi`-decorated function stub.
    ///
    /// These have no Ridge expression body; the codegen layer emits a thin
    /// wrapper function that delegates directly to the specified BEAM target
    /// via `call 'beam_module':'beam_fn'(args...)`.
    ///
    /// Emitted by `ridge-lower` for every `Body::Ffi` item so that
    /// same-module pure-Ridge callers can reference them as local functions
    /// without `erlc +from_core` rejecting the Core Erlang module with
    /// "undefined function X/N".
    Ffi(IrFfiFn),
}

/// A lowered function declaration.
#[derive(Debug, Clone)]
pub struct IrFn {
    /// The function's source-level name.
    pub name: String,
    /// The module this function belongs to.
    pub module: ModuleId,
    /// The function's parameters (each carries a name and type).
    pub params: Vec<IrParam>,
    /// The function's return type.
    pub ret_ty: Type,
    /// Phase 4 inferred capability set, post-check.
    pub caps: CapabilitySet,
    /// Generalised type scheme of the declaration.
    pub scheme: Scheme,
    /// The lowered function body.
    pub body: IrExpr,
    /// AST `FnDecl` `NodeId` for diagnostics.
    pub origin: NodeId,
    /// Source span of the function declaration.
    pub span: Span,
    /// Whether this function is `pub`.
    pub is_pub: bool,
    /// Canonical-main marker (D059).
    pub is_main: bool,
    /// Doc-comment text if any (D067).
    pub doc: Option<String>,
}

/// A single function parameter.
#[derive(Debug, Clone)]
pub struct IrParam {
    /// The parameter's source-level name.
    pub name: String,
    /// The parameter's type.
    pub ty: Type,
    /// Source span of the parameter.
    pub span: Span,
}

/// A lowered top-level constant declaration.
#[derive(Debug, Clone)]
pub struct IrConst {
    /// The constant's source-level name.
    pub name: String,
    /// The constant's type.
    pub ty: Type,
    /// The constant's lowered value expression.
    pub value: IrExpr,
    /// AST `ConstDecl` `NodeId` for diagnostics.
    pub origin: NodeId,
    /// Source span of the constant declaration.
    pub span: Span,
    /// Whether this constant is `pub`.
    pub is_pub: bool,
}

/// A lowered `@ffi`-decorated function stub.
///
/// The codegen layer emits a thin wrapper function that delegates to the
/// specified foreign module and function (e.g. `erlang:trunc/1`).
#[derive(Debug, Clone)]
pub struct IrFfiFn {
    /// The Ridge-side function name (e.g. `"truncate"`).
    pub name: String,
    /// The foreign module to call (e.g. `"erlang"`).
    /// Named `ffi_module` to stay target-neutral at the IR layer.
    pub ffi_module: String,
    /// The foreign function name to call (e.g. `"trunc"`).
    /// Named `ffi_fn` to stay target-neutral at the IR layer.
    pub ffi_fn: String,
    /// Ridge-side parameter names for the wrapper function.
    ///
    /// The wrapper function signature has `params.len()` arguments.
    /// However, only the first `ffi_call_arity` of them are forwarded to the
    /// foreign call target — the remainder are dummy Ridge-convention params
    /// (e.g. the `_unit: Unit` slot for 0-arity foreign functions).
    pub params: Vec<String>,
    /// Number of arguments forwarded to the foreign call target.
    ///
    /// Typically equals `params.len()`, but for 0-arity foreign functions
    /// wrapped with a Ridge `_unit: Unit` dummy param, `ffi_call_arity == 0`
    /// while `params.len() == 1`.
    pub ffi_call_arity: u32,
    /// Whether this stub is `pub` (exported from the generated target module).
    pub is_pub: bool,
    /// Source span of the original `@ffi` declaration.
    pub span: Span,
}
