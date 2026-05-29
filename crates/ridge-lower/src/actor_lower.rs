//! Actor lowering — §4.14 / §8.1.
//!
//! # Rule summary
//!
//! `lower_actor` converts an AST `ActorDecl` into a flat `IrActor` dispatch
//! shape.  The three structural phases mirror the actor's member list:
//!
//! 1. **State fields** — each `state` member becomes an `IrStateField`.
//!    Default expressions are lowered in *non*-actor-body context (they are
//!    evaluated at declaration time, not at handler-dispatch time).
//!
//! 2. **Init block** — the optional `init` member becomes an `IrInit`.
//!    Its body is lowered with `ctx.in_actor_body = true` and
//!    `ctx.current_state_fields` populated, so `<-` assignments inside
//!    the init body are classified as `AssignTarget::StateField`.
//!
//! 3. **Handlers** — each `on` member becomes an `IrHandler`.
//!    Same actor-body-context flag pattern as init.
//!
//! # State-field classification (R8)
//!
//! `ridge-resolve` has no `Binding::StateField` variant — adding one would
//! violate the upstream-crate constraint in §1.3.  Instead, `lower_actor`
//! populates `ctx.current_state_fields` with the actor's state-field names
//! before lowering each body.  `block::lower_assign` consults this set to
//! classify `AssignTarget::StateField` vs `AssignTarget::Local`.
//!
//! A save/restore pattern is used around each body so that nested actors
//! (disallowed by Phase 4, but defensively handled) do not corrupt the
//! enclosing state.
//!
//! # Deferred wiring (T17)
//!
//! - `IrStateField.ty` and `IrHandler.ret_ty` are `Type::Error` placeholders;
//!   the resolved types live in `node_types` and will be wired in T17.
//! - `IrActor.tycon` is a `TyConId(0)` placeholder; authoritative tycon lookup
//!   is deferred to T17 (see `lookup_actor_tycon`).
//! - `caps_from_ast` returns `CapabilitySet::PURE`; actual inference wiring
//!   is T17 (see `inferred_caps` side-table).
//! - `IrActor.origin` and `IrHandler.origin` are `NodeId(0)` placeholders;
//!   `ActorDecl` items carry no `NodeId` per the side-table convention.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::todo)
)]

use ridge_ast::{
    decl::{ActorDecl, ActorMember, InitDecl, OnHandler, StateDecl},
    Expr, Param, Span,
};
use ridge_ir::{
    actor::{IrActor, IrHandler, IrInit, IrStateField, MailboxConfig, MailboxPolicy},
    IrParam,
};
use ridge_resolve::{NodeId, NodeKind};
use ridge_types::{CapabilitySet, TyConId, Type};
use rustc_hash::FxHashSet;

use crate::ast_type::lower_ast_type;
use crate::block::lower_block;
use crate::core::lower_expr;
use crate::ctx::LowerCtx;

// ── Public entry point ────────────────────────────────────────────────────────

/// Lower an AST [`ActorDecl`] to a flat [`IrActor`] dispatch shape (§8.1).
///
/// The returned `IrActor` carries placeholder values for fields that require
/// T17 wiring (`tycon`, field types, capability sets).  See the module-level
/// documentation for the full list of deferred items.
pub fn lower_actor(ctx: &mut LowerCtx<'_>, decl: &ActorDecl) -> IrActor {
    // ── 1. Collect state-field names (used for AssignTarget classification) ────
    let state_field_names: FxHashSet<String> = decl
        .members
        .iter()
        .filter_map(|m| {
            if let ActorMember::State(s) = m {
                Some(s.name.text.clone())
            } else {
                None
            }
        })
        .collect();

    // ── 2. Lower state fields (non-actor-body context) ────────────────────────
    let state_fields: Vec<IrStateField> = decl
        .members
        .iter()
        .filter_map(|m| {
            if let ActorMember::State(s) = m {
                Some(lower_state_decl(ctx, s))
            } else {
                None
            }
        })
        .collect();

    // ── 3. Lower optional init block ─────────────────────────────────────────
    let init: Option<IrInit> = decl.members.iter().find_map(|m| {
        if let ActorMember::Init(i) = m {
            Some(lower_init_decl(ctx, i, &state_field_names))
        } else {
            None
        }
    });

    // ── 4. Lower handlers (dispatch table) ───────────────────────────────────
    let dispatch: Vec<IrHandler> = decl
        .members
        .iter()
        .filter_map(|m| {
            if let ActorMember::On(h) = m {
                Some(lower_on_handler(ctx, h, &state_field_names))
            } else {
                None
            }
        })
        .collect();

    // ── 5. Lower the optional mailbox configuration ──────────────────────────
    let mailbox_config: Option<MailboxConfig> = decl.members.iter().find_map(|m| {
        if let ActorMember::Mailbox(mb) = m {
            Some(lower_mailbox_config(&mb.config))
        } else {
            None
        }
    });

    IrActor {
        name: decl.name.text.clone(),
        module: ctx.module_id,
        tycon: lookup_actor_tycon(ctx, &decl.name.text),
        state_fields,
        init,
        dispatch,
        mailbox_config,
        // ActorDecl items carry no NodeId per the side-table convention.
        origin: NodeId(0),
        span: decl.span,
        is_pub: matches!(decl.vis, ridge_ast::Visibility::Pub),
        doc: decl.doc.as_ref().map(|d| d.text.clone()),
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Translate the AST mailbox config into its IR mirror. The two enums share
/// shape and names; the function only re-tags the variants so that downstream
/// codegen consumes a single `ridge_ir::MailboxConfig` regardless of source.
const fn lower_mailbox_config(ast: &ridge_ast::MailboxConfig) -> MailboxConfig {
    match ast {
        ridge_ast::MailboxConfig::Unbounded => MailboxConfig::Unbounded,
        ridge_ast::MailboxConfig::Bounded { capacity, policy } => MailboxConfig::Bounded {
            capacity: *capacity,
            policy: match policy {
                ridge_ast::MailboxPolicy::DropNewest => MailboxPolicy::DropNewest,
                ridge_ast::MailboxPolicy::DropOldest => MailboxPolicy::DropOldest,
                ridge_ast::MailboxPolicy::Error => MailboxPolicy::Error,
            },
        },
    }
}

/// Lower a `state` field declaration to `IrStateField`.
///
/// Default expressions are lowered in *non*-actor-body context: they are
/// evaluated at declaration time, so `<-` targeting is not active.
///
/// The state field type is lowered from the declared AST annotation via
/// [`lower_ast_type`].
fn lower_state_decl(ctx: &mut LowerCtx<'_>, s: &StateDecl) -> IrStateField {
    let ty = lower_ast_type(ctx, &s.ty);
    IrStateField {
        name: s.name.text.clone(),
        ty,
        default: s.default.as_ref().map(|e| lower_expr(ctx, e)),
        span: s.span,
    }
}

/// Lower the `init` block of an actor.
///
/// Sets `ctx.in_actor_body = true` and installs `state_field_names` into
/// `ctx.current_state_fields` before lowering the body block, then restores
/// both fields (save/restore pattern).
///
/// # Capability set
///
/// Uses the declared AST capability list (`InitDecl.caps`) converted to a
/// `CapabilitySet`.  This is the syntactic declaration, not the inferred set —
/// `inferred_caps` in Phase 4 only covers top-level `fn` decls, not `init`
/// blocks.  Actor init caps are stored in `ActorSchema.init_caps` inside the
/// `TyConArena`; reading them by name lookup is possible but adds complexity
/// beyond the scope of this wiring.  The declared set is a faithful 1:1 copy
/// of what the user wrote and is correct for well-typed programs.
fn lower_init_decl(
    ctx: &mut LowerCtx<'_>,
    i: &InitDecl,
    state_field_names: &FxHashSet<String>,
) -> IrInit {
    let saved_in_actor_body = ctx.in_actor_body;
    let saved_state_fields = ctx.current_state_fields.take();

    ctx.in_actor_body = true;
    ctx.current_state_fields = Some(state_field_names.clone());

    let body = lower_block(ctx, &i.body);

    ctx.in_actor_body = saved_in_actor_body;
    ctx.current_state_fields = saved_state_fields;

    IrInit {
        // Bare-param types use structural lookup via init span.
        params: i
            .params
            .iter()
            .enumerate()
            .map(|(idx, p)| param_to_ir_param(ctx, i.span, idx, p))
            .collect(),
        caps: caps_from_ast_decl(&i.caps),
        body,
        span: i.span,
    }
}

/// Lower an `on` handler to `IrHandler`.
///
/// Sets `ctx.in_actor_body = true` and installs `state_field_names` into
/// `ctx.current_state_fields` before lowering the handler body expression,
/// then restores both fields (save/restore pattern).
///
/// # Types and capabilities
///
/// - `ret_ty` — lowered from the declared `OnHandler.ret` annotation via
///   [`lower_ast_type`]; falls back to `Type::Error` when no annotation is
///   present.
/// - `caps` — derived from the declared AST capability list (`OnHandler.caps`).
///   Handler caps are NOT in the `inferred_caps` side-table (which only covers
///   top-level `fn` decls); the declared set is the correct source for handlers.
fn lower_on_handler(
    ctx: &mut LowerCtx<'_>,
    h: &OnHandler,
    state_field_names: &FxHashSet<String>,
) -> IrHandler {
    let saved_in_actor_body = ctx.in_actor_body;
    let saved_state_fields = ctx.current_state_fields.take();

    ctx.in_actor_body = true;
    ctx.current_state_fields = Some(state_field_names.clone());

    let body = lower_expr(ctx, &h.body);

    ctx.in_actor_body = saved_in_actor_body;
    ctx.current_state_fields = saved_state_fields;

    // Resolve ret_ty from declared annotation when present;
    // when absent read the body's inferred type from node_types.
    let ret_ty = if let Some(ast_ty) = &h.ret {
        lower_ast_type(ctx, ast_ty)
    } else {
        // Read the handler body's inferred return type from node_types.
        // Handler bodies are single expressions (NodeKind::Expr) or blocks.
        let bkind = match &h.body {
            Expr::Block(_) => NodeKind::Block,
            Expr::Try { .. } => NodeKind::Try,
            _ => NodeKind::Expr,
        };
        let bspan = match &h.body {
            Expr::Block(b) => b.span,
            Expr::Try { span, .. } => *span,
            other => other.span(),
        };
        ctx.node_id_map
            .as_ref()
            .and_then(|m| m.get(bspan, bkind))
            .and_then(|nid| ctx.node_type(nid).cloned())
            .unwrap_or(Type::Error)
    };

    IrHandler {
        message_name: h.name.text.clone(),
        // Bare-param types use structural lookup via handler span.
        params: h
            .params
            .iter()
            .enumerate()
            .map(|(idx, p)| param_to_ir_param(ctx, h.span, idx, p))
            .collect(),
        ret_ty,
        caps: caps_from_ast_decl(&h.caps),
        body,
        // OnHandler items carry no NodeId per the side-table convention.
        origin: NodeId(0),
        span: h.span,
        doc: h.doc.as_ref().map(|d| d.text.clone()),
    }
}

/// Convert an AST [`Param`] to an [`IrParam`].
///
/// For `Param::Annotated` the declared type annotation is lowered via
/// [`lower_ast_type`].  For `Param::Bare` (no annotation) the type is resolved
/// by looking up the enclosing declaration's `Type::Fn` at
/// `(decl_span, NodeKind::Expr)` and indexing `params[param_idx]`.  For actor
/// handlers and init blocks the expression node stores their evaluated type
/// (not a `Fn`), so this falls back to `Type::Error` — but uses the correct
/// structural pattern (same as lambdas in `crate::core`).
///
/// Bare param type lifted from enclosing decl's `Type::Fn` (structural pattern).
fn param_to_ir_param(
    ctx: &mut LowerCtx<'_>,
    decl_span: Span,
    param_idx: usize,
    param: &Param,
) -> IrParam {
    match param {
        Param::Bare(ident) => {
            // Look up the decl's Type::Fn from (decl_span, NodeKind::Expr)
            // and extract params[param_idx].  Actor handler/init decl spans do not
            // store a Type::Fn in node_types, so this falls back to Type::Error.
            let ty = ctx
                .node_id_map
                .as_ref()
                .and_then(|m| m.get(decl_span, NodeKind::Expr))
                .and_then(|nid| ctx.node_type(nid).cloned())
                .and_then(|fn_ty| {
                    if let Type::Fn { params, .. } = fn_ty {
                        params.into_iter().nth(param_idx)
                    } else {
                        None
                    }
                })
                .unwrap_or(Type::Error);
            IrParam {
                name: ident.text.clone(),
                ty,
                span: ident.span,
            }
        }
        Param::Annotated { name, ty, span } => IrParam {
            name: name.text.clone(),
            ty: lower_ast_type(ctx, ty),
            span: *span,
        },
    }
}

/// Convert a slice of AST [`ridge_ast::Capability`] values to a [`CapabilitySet`].
///
/// This converts the declared syntactic capability list to the semantic
/// `CapabilitySet`.  For top-level `fn` decls this is supplemented (or replaced)
/// by `lookup_inferred_caps`; for handler/init decls this is the authoritative
/// source because `inferred_caps` only covers top-level `fn` decls.
fn caps_from_ast_decl(caps: &[ridge_ast::Capability]) -> CapabilitySet {
    let mut cs = CapabilitySet::PURE;
    for &c in caps {
        cs.insert(c);
    }
    cs
}

/// Look up the `TyConId` for an actor by name from the workspace's tycon arena.
///
/// Uses [`LowerCtx::lookup_tycon_by_name`], which builds a name→`TyConId` cache
/// on first call.  Falls back to `TyConId(0)` when the workspace is absent or
/// no matching tycon is found (the actor name is not registered in the arena).
fn lookup_actor_tycon(ctx: &mut LowerCtx<'_>, name: &str) -> TyConId {
    ctx.lookup_tycon_by_name(name).unwrap_or(TyConId(0))
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{
        decl::{ActorDecl, ActorMember, InitDecl, OnHandler, StateDecl},
        Block, Expr, Ident, Literal, Span, Visibility,
    };
    use ridge_ir::{AssignTarget, IrExpr};
    use ridge_resolve::{BindingMap, LocalId, ModuleId, NodeIdMap, NodeKind};

    fn sp() -> Span {
        Span::point(0)
    }

    fn sp_at(start: u32, end: u32) -> Span {
        Span::new(start, end)
    }

    fn fresh_ctx() -> LowerCtx<'static> {
        LowerCtx::new(ModuleId(0), &[])
    }

    fn ident(text: &str) -> Ident {
        Ident {
            text: text.into(),
            span: sp(),
        }
    }

    fn int_lit(n: &str) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.into(),
            span: sp(),
        })
    }

    // ── actor with no init, all-default state fields, single handler ────────────
    //
    // actor Counter =
    //     state count: Int = 0
    //     on get -> Int = count
    //
    // Verifies: state_fields[0].name == "count", init is None, dispatch len == 1.
    #[test]
    fn actor_no_init_single_handler() {
        let mut ctx = fresh_ctx();

        let decl = ActorDecl {
            vis: Visibility::Private,
            name: ident("Counter"),
            members: vec![
                ActorMember::State(StateDecl {
                    name: ident("count"),
                    ty: ridge_ast::Type::Named {
                        name: ident("Int"),
                        span: sp(),
                    },
                    default: Some(int_lit("0")),
                    span: sp(),
                }),
                ActorMember::On(OnHandler {
                    caps: vec![],
                    name: ident("get"),
                    params: vec![],
                    ret: Some(ridge_ast::Type::Named {
                        name: ident("Int"),
                        span: sp(),
                    }),
                    // Use a literal body to avoid requiring a binding map in this
                    // unit test (ident resolution requires NodeIdMap + BindingMap).
                    body: int_lit("0"),
                    span: sp(),
                    doc: None,
                }),
            ],
            span: sp(),
            doc: None,
        };

        let actor = lower_actor(&mut ctx, &decl);

        assert_eq!(actor.name, "Counter");
        assert_eq!(actor.state_fields.len(), 1);
        assert_eq!(actor.state_fields[0].name, "count");
        assert!(
            actor.state_fields[0].default.is_some(),
            "default must be lowered"
        );
        assert!(actor.init.is_none(), "no init block expected");
        assert_eq!(actor.dispatch.len(), 1);
        assert_eq!(actor.dispatch[0].message_name, "get");
        assert!(!actor.is_pub);
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
        // in_actor_body and current_state_fields must be restored after lowering.
        assert!(
            !ctx.in_actor_body,
            "in_actor_body must be reset after lower_actor"
        );
        assert!(
            ctx.current_state_fields.is_none(),
            "current_state_fields must be reset"
        );
    }

    // ── actor with init block + multi-arg state ──────────────────────────────────
    //
    // actor RateLimiter =
    //     state capacity: Int
    //     state tokens: Int
    //     init (cap: Int) =
    //         capacity <- cap
    //         tokens   <- cap
    //
    // Verifies: state_fields.len() == 2, init.is_some(), init.params.len() == 1.
    #[test]
    fn actor_with_init_and_multi_state() {
        let mut ctx = fresh_ctx();

        // We need a binding map for the `capacity` and `tokens` assignment targets
        // in the init body.  Since the init body is a Block with two Assign stmts,
        // and both target state-field names (not Local bindings), the classification
        // is driven by current_state_fields — no binding map needed.
        // However lower_assign also looks for a NodeId; when there's none it emits
        // a defensive L999.  To keep this test clean we use Expr::Unit as the init
        // body instead of real assignments.
        let init_body = Block {
            stmts: vec![Expr::Unit(sp())],
            span: sp(),
        };

        let decl = ActorDecl {
            vis: Visibility::Pub,
            name: ident("RateLimiter"),
            members: vec![
                ActorMember::State(StateDecl {
                    name: ident("capacity"),
                    ty: ridge_ast::Type::Named {
                        name: ident("Int"),
                        span: sp(),
                    },
                    default: None,
                    span: sp(),
                }),
                ActorMember::State(StateDecl {
                    name: ident("tokens"),
                    ty: ridge_ast::Type::Named {
                        name: ident("Int"),
                        span: sp(),
                    },
                    default: None,
                    span: sp(),
                }),
                ActorMember::Init(InitDecl {
                    caps: vec![],
                    params: vec![Param::Bare(Ident {
                        text: "cap".into(),
                        span: sp(),
                    })],
                    body: init_body,
                    span: sp(),
                }),
            ],
            span: sp(),
            doc: None,
        };

        let actor = lower_actor(&mut ctx, &decl);

        assert_eq!(actor.name, "RateLimiter");
        assert!(actor.is_pub, "actor should be pub");
        assert_eq!(actor.state_fields.len(), 2);
        assert_eq!(actor.state_fields[0].name, "capacity");
        assert_eq!(actor.state_fields[1].name, "tokens");
        assert!(actor.init.is_some(), "init block must be lowered");

        let init = actor.init.unwrap();
        assert_eq!(init.params.len(), 1);
        assert_eq!(init.params[0].name, "cap");
        assert_eq!(actor.dispatch.len(), 0, "no handlers expected");
        // Context must be restored.
        assert!(!ctx.in_actor_body);
        assert!(ctx.current_state_fields.is_none());
    }

    // ── actor with multiple handlers (multi-handler dispatch shape) ──────────────
    //
    // actor Counter =
    //     state count: Int = 0
    //     on increment = ()
    //     on decrement = ()
    //     on get -> Int = count
    //
    // Verifies: dispatch.len() == 3, message names in order.
    #[test]
    fn actor_multiple_handlers_dispatch_shape() {
        let mut ctx = fresh_ctx();

        let decl = ActorDecl {
            vis: Visibility::Private,
            name: ident("Counter"),
            members: vec![
                ActorMember::State(StateDecl {
                    name: ident("count"),
                    ty: ridge_ast::Type::Named {
                        name: ident("Int"),
                        span: sp(),
                    },
                    default: Some(int_lit("0")),
                    span: sp(),
                }),
                ActorMember::On(OnHandler {
                    caps: vec![],
                    name: ident("increment"),
                    params: vec![],
                    ret: None,
                    body: Expr::Unit(sp()),
                    span: sp(),
                    doc: None,
                }),
                ActorMember::On(OnHandler {
                    caps: vec![],
                    name: ident("decrement"),
                    params: vec![],
                    ret: None,
                    body: Expr::Unit(sp()),
                    span: sp(),
                    doc: None,
                }),
                ActorMember::On(OnHandler {
                    caps: vec![],
                    name: ident("get"),
                    params: vec![],
                    ret: Some(ridge_ast::Type::Named {
                        name: ident("Int"),
                        span: sp(),
                    }),
                    body: int_lit("0"),
                    span: sp(),
                    doc: None,
                }),
            ],
            span: sp(),
            doc: None,
        };

        let actor = lower_actor(&mut ctx, &decl);

        assert_eq!(actor.dispatch.len(), 3);
        assert_eq!(actor.dispatch[0].message_name, "increment");
        assert_eq!(actor.dispatch[1].message_name, "decrement");
        assert_eq!(actor.dispatch[2].message_name, "get");
        // All handlers get Type::Error as placeholder (T17 deferred).
        assert!(matches!(actor.dispatch[0].ret_ty, Type::Error));
        assert!(matches!(actor.dispatch[1].ret_ty, Type::Error));
        assert!(matches!(actor.dispatch[2].ret_ty, Type::Error));
        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );
    }

    // ── state-field assignment classifies as AssignTarget::StateField ────────────
    //
    // Actor with `state count: Int = 0` and handler body `count <- 1`.
    // The assignment target `count` must become AssignTarget::StateField.
    //
    // To drive lower_assign correctly we need:
    // - A NodeIdMap with the target ident span registered.
    // - A BindingMap with Binding::Local for that node (simulate how resolve works).
    // - ctx.in_actor_body = true and ctx.current_state_fields = {"count"}.
    //
    // The StateField classification takes priority over the binding-map result
    // because is_state_field is checked first in lower_assign.
    #[test]
    fn state_field_assignment_classifies_as_state_field() {
        let target_span = sp_at(10, 15);

        // Register the target ident span in the NodeIdMap.
        let mut nid_map = NodeIdMap::default();
        let node_id = nid_map.assign(target_span, NodeKind::Ident).unwrap();

        // Give it a Local binding (what ridge-resolve would produce).
        let local_id = LocalId(0);
        let mut binding_map: BindingMap = vec![None; (node_id.0 + 1) as usize];
        binding_map[node_id.0 as usize] = Some(ridge_resolve::imports::Binding::Local(local_id));

        let mut ctx = LowerCtx::new(ModuleId(0), &[]);
        ctx.attach_bindings(nid_map, Box::leak(Box::new(binding_map)));

        // Directly set actor-body context (simulating what lower_actor does).
        ctx.in_actor_body = true;
        let mut fields = FxHashSet::default();
        fields.insert("count".to_string());
        ctx.current_state_fields = Some(fields);

        // Build the assignment: `count <- 1`.
        let target = Expr::Ident(Ident {
            text: "count".into(),
            span: target_span,
        });
        let value = int_lit("1");
        let span = sp_at(10, 20);

        let ir = crate::block::lower_assign(&mut ctx, &target, &value, span);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got: {:?}",
            ctx.errors
        );

        match ir {
            IrExpr::Assign { target, value, .. } => {
                match target {
                    AssignTarget::StateField { name, .. } => {
                        assert_eq!(name, "count", "must be StateField(count)");
                    }
                    AssignTarget::Local { name, .. } => {
                        panic!("expected StateField, got Local({name:?})");
                    }
                }
                match *value {
                    IrExpr::Lit {
                        value: ridge_ir::IrLit::Int(1),
                        ..
                    } => {}
                    other => panic!("expected Lit Int 1, got {other:?}"),
                }
            }
            other => panic!("expected IrExpr::Assign, got {other:?}"),
        }
    }
}
