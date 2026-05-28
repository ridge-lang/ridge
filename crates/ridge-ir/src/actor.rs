//! Actor-related IR nodes.

use crate::expr::IrExpr;
use crate::item::IrParam;
use ridge_ast::Span;
use ridge_resolve::{ModuleId, NodeId};
use ridge_types::{CapabilitySet, TyConId, Type};

/// An actor as a flat dispatch shape.
///
/// Source `actor X = state ... on m1 ... on m2 ...` lowers to:
///
/// ```ignore
/// IrActor {
///     state_fields:  [s1, s2, ...],            // declared `state` fields
///     init:          Option<IrInit>,           // present iff source had `init`
///     dispatch:      Vec<IrHandler>,           // one entry per `on` handler
/// }
/// ```
///
/// **There is no `IrExpr` variant for an `OnHandler` body** — handlers are
/// flat siblings of the actor.  Each handler carries a discriminator
/// (`message_name`) so backends can compile a tag-based dispatch (BEAM
/// `receive`, `gen_server` `handle_call/cast`, native message-pump).
#[derive(Debug, Clone)]
pub struct IrActor {
    /// The actor's source-level name.
    pub name: String,
    /// The module this actor belongs to.
    pub module: ModuleId,
    /// The actor's type-constructor ID.
    pub tycon: TyConId,
    /// The actor's declared state fields.
    pub state_fields: Vec<IrStateField>,
    /// The actor's `init` block, if present.
    pub init: Option<IrInit>,
    /// One handler per `on` clause, in source order.
    pub dispatch: Vec<IrHandler>,
    /// Mailbox configuration (cut 0.2.7).
    ///
    /// `None` means no `mailbox` member was declared and the actor defaults
    /// to the historical unbounded semantics. `Some(MailboxConfig::Unbounded)`
    /// means an explicit `mailbox unbounded` member was declared. The two are
    /// semantically equivalent; the field preserves source fidelity.
    pub mailbox_config: Option<MailboxConfig>,
    /// AST `ActorDecl` `NodeId` for diagnostics.
    pub origin: NodeId,
    /// Source span of the actor declaration.
    pub span: Span,
    /// Whether this actor is `pub`.
    pub is_pub: bool,
    /// Doc-comment text if any (D067).
    pub doc: Option<String>,
}

/// Mailbox capacity and overflow policy of an actor (cut 0.2.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MailboxConfig {
    /// Unbounded mailbox — senders never block, never fail.
    Unbounded,
    /// `bounded N <policy>` — capacity-bounded mailbox.
    Bounded {
        /// Capacity. Always `>= 1` and representable as `i64`.
        capacity: i64,
        /// Overflow policy.
        policy: MailboxPolicy,
    },
}

/// Mailbox overflow policy (cut 0.2.7).
///
/// `DropOldest` is accepted by the parser but rejected by the typechecker
/// until the broker mechanism ships in a later cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxPolicy {
    /// Silently drop the incoming message on overflow.
    DropNewest,
    /// Drop the head-of-queue message on overflow; not yet implemented.
    DropOldest,
    /// Signal failure to the sender on overflow.
    Error,
}

/// A single actor state field.
#[derive(Debug, Clone)]
pub struct IrStateField {
    /// The field's source-level name.
    pub name: String,
    /// The field's type.
    pub ty: Type,
    /// `Some` for fields with a literal default expression at decl time;
    /// `None` for fields that **must** be initialised by `init` (D061).
    /// Lowering preserves the default expression as an IR expression so
    /// codegen can emit it without re-typing.
    pub default: Option<IrExpr>,
    /// Source span of the state field declaration.
    pub span: Span,
}

/// The lowered `init` block of an actor.
#[derive(Debug, Clone)]
pub struct IrInit {
    /// The `init` block's parameters.
    pub params: Vec<IrParam>,
    /// Capability set of the `init` block.
    pub caps: CapabilitySet,
    /// The init body as a sequence of state assignments + arbitrary expressions.
    /// Lowering rule: each `<state-field> <- <expr>` becomes an
    /// `IrExpr::AssignStateField { field_name, value }`; arbitrary expressions
    /// stay as plain `IrExpr` siblings.
    pub body: IrExpr, // wrapped in IrExpr::Block if multi-stmt
    /// Source span of the `init` block.
    pub span: Span,
}

/// A single `on`-handler of an actor.
#[derive(Debug, Clone)]
pub struct IrHandler {
    /// Tag used by codegen for dispatch (the source-level `on m` name).
    pub message_name: String,
    /// The handler's parameters.
    pub params: Vec<IrParam>,
    /// The handler's return type.
    pub ret_ty: Type,
    /// Capability set of this handler.
    pub caps: CapabilitySet,
    /// The handler's lowered body.
    pub body: IrExpr,
    /// AST `OnHandler` `NodeId` for diagnostics.
    pub origin: NodeId,
    /// Source span of the handler.
    pub span: Span,
    /// Doc-comment text if any (D067).
    pub doc: Option<String>,
}
