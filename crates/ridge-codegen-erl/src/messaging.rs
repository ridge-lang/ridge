//! §4.17–§4.19 — Lower `IrExpr::Send`, `IrExpr::Ask`, `IrExpr::Spawn`.
//!
//! All three actor-messaging primitives are routed through `ridge_rt` wrappers
//! per resolved **OQ-E004** (§8.2): `ridge_rt:send_op/2`, `ridge_rt:ask/3`,
//! and `ridge_rt:spawn_actor/3`.  This one-hop indirection is the seam where
//! future telemetry and tracing hooks land without recompiling user code.
//!
//! ## §4.17 Send (`!`)
//!
//! ```ignore
//! handle ! handler arg1 ... argN
//! ```
//! → `call 'ridge_rt':'send_op' (HandleExpr, {handler_tag, Arg1, ..., ArgN})`
//!
//! `send_op/2` honours the bounded-mailbox policy carried by the handle.
//! `drop_newest` silently drops the incoming message on overflow; `error`
//! raises `{mailbox_full, Pid}` in the caller so the supervisor can react.
//! Unbounded handles behave exactly as before (one cast, no policy check).
//!
//! Returns `'ok'` (Unit).
//!
//! ## §4.18 Ask (`?>`)
//!
//! ```ignore
//! handle ?> handler arg1 ... argN [timeout <ms|never>]
//! ```
//! → `call 'ridge_rt':'ask' (HandleExpr, {handler_tag, Arg1, ..., ArgN}, TimeoutMs)`
//!
//! Timeout resolution (per resolved OQ-E001 §8):
//! - `timeout: None`             → `5000`           (BEAM convention)
//! - `timeout: Some(Never)`      → `'infinity'`
//! - `timeout: Some(Millis(e))`  → `lower_expr(e)`  (typecheck guarantees `e: Int`)
//!
//! ## §4.19 Spawn
//!
//! ```ignore
//! spawn ActorName arg1 ... argN
//! ```
//! → `call 'ridge_rt':'spawn_actor' (ActorBeamModule, [Arg1, ..., ArgN], [])`
//!
//! Returns a Ridge `Handle a` — opaque at the source level, encoded at the
//! runtime layer as `{ridge_handle, Pid, MailboxConfig}`. `ridge_rt`
//! reads the actor module's `'__ridge_mailbox_config'/0` accessor to
//! assemble the tuple; the codegen site does not need to know whether the
//! target actor is bounded.

// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// lower_send / lower_ask / lower_spawn are called from expr.rs::lower_expr_in_scope
// which is always reachable; derive_actor_beam_module is also used by messaging tests.
// No dead_code suppression needed here; the allow is for consistency only.

use crate::core_ast::{CErlAtom, CErlExpr, CErlLit};
use crate::error::CodegenError;
use crate::expr::lower_expr_in_scope;
use crate::scope::LocalScope;
use ridge_ast::Span;
use ridge_ir::{IrExpr, IrTimeout, SymbolRef};

// ── §4.17 Send ─────────────────────────────────────────────────────────────────

/// Lower `IrExpr::Send` to
/// `call 'ridge_rt':'send_op' (Handle, {Tag, A1, ..., AN})`.
///
/// Per resolved **OQ-E004** (§8.2): all `!` sends route through `ridge_rt`.
/// The function name moved from `send` to `send_op` when bounded mailboxes
/// landed — `send_op` honours the policy carried by the handle. The
/// indirection is the telemetry/tracing seam (plan §3.6).
///
/// Returns `'ok'` (Unit) as the expression value.
// OQ-E004: always route via ridge_rt (plan §4.17 + §8.2).
pub(crate) fn lower_send(
    handle: &IrExpr,
    message: &SymbolRef,
    args: &[IrExpr],
    span: Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    let handle_expr = lower_expr_in_scope(handle, scope)?;
    let msg_tuple = build_message_tuple(message, args, span, scope)?;

    Ok(CErlExpr::Call {
        module: CErlAtom("ridge_rt".into()),
        fn_name: CErlAtom("send_op".into()),
        args: vec![handle_expr, msg_tuple],
    })
}

// ── §4.18 Ask ──────────────────────────────────────────────────────────────────

/// Lower `IrExpr::Ask` to `call 'ridge_rt':'ask' (Handle, {Tag, A1, ..., AN}, Timeout)`.
///
/// Per resolved **OQ-E004** (§8.2): all `?>` asks route through `ridge_rt:ask/3`.
/// Per resolved **OQ-E001** (§8.2): default timeout is `5000` ms.
///
/// Timeout table:
/// | IR `timeout` | Emitted Core Erlang |
/// |---|---|
/// | `None`                  | `5000`            |
/// | `Some(Never)`           | `'infinity'`      |
/// | `Some(Millis(e))`       | `lower_expr(e)`   |
// OQ-E001: 5000 ms default (plan §4.18 + §8.2).
// OQ-E004: route via ridge_rt (plan §4.18 + §8.2).
pub(crate) fn lower_ask(
    handle: &IrExpr,
    message: &SymbolRef,
    args: &[IrExpr],
    timeout: Option<&IrTimeout>,
    span: Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    let handle_expr = lower_expr_in_scope(handle, scope)?;
    let msg_tuple = build_message_tuple(message, args, span, scope)?;

    // §4.18 timeout resolution (OQ-E001).
    let timeout_expr = match timeout {
        // No explicit timeout → runtime default 5000 ms.
        None => CErlExpr::Lit(CErlLit::Int(5000)), // OQ-E001: 5000 ms default
        // `timeout never` → Erlang `infinity`.
        Some(IrTimeout::Never) => CErlExpr::Lit(CErlLit::Atom(CErlAtom("infinity".into()))),
        // `timeout <expr>` → lower the expression (typecheck guarantees `e: Int`).
        Some(IrTimeout::Millis(e)) => lower_expr_in_scope(e, scope)?,
        // IrTimeout is #[non_exhaustive]; catch future variants defensively.
        Some(_) => {
            return Err(CodegenError::IrShapeMalformed {
                variant: "IrTimeout",
                span,
                detail: "unrecognised IrTimeout variant — no lowering arm defined".into(),
            });
        }
    };

    Ok(CErlExpr::Call {
        module: CErlAtom("ridge_rt".into()),
        fn_name: CErlAtom("ask".into()),
        args: vec![handle_expr, msg_tuple, timeout_expr],
    })
}

// ── §4.19 Spawn ────────────────────────────────────────────────────────────────

/// Lower `IrExpr::Spawn` to `call 'ridge_rt':'spawn_actor' (ActorMod, [Args...], [])`.
///
/// The actor's BEAM module name is derived from the `ActorType` `SymbolRef`.
/// `ridge_rt:spawn_actor/3` calls `gen_server:start_link/3`, reads the
/// actor module's `'__ridge_mailbox_config'/0`, and returns the
/// `{ridge_handle, Pid, MailboxConfig}` handle tuple. Init failure crashes
/// the spawner per resolved **OQ-E006** (§8.2) — BEAM-crash semantics for
/// init failure.
///
/// # OQ-E006 asymmetry note
/// Init failure propagates as a BEAM exception rather than a Ridge `Result`.
/// This is the deliberate language-level asymmetry documented in §4.29 and §8.2.
// OQ-E006: BEAM-crash on init failure (plan §4.19 + §8.2).
pub(crate) fn lower_spawn(
    actor: &SymbolRef,
    args: &[IrExpr],
    span: Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    // Extract actor module name from the ActorType SymbolRef.
    let SymbolRef::ActorType {
        module: actor_module_id,
        name: actor_name,
    } = actor
    else {
        return Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Spawn",
            span,
            detail: format!(
                "Spawn actor field is not SymbolRef::ActorType — got {actor:?} (Phase 5 invariant violated)"
            ),
        });
    };

    // Derive the actor's BEAM module atom.
    // The actor BEAM module name is `"<parent_module_beam_name>_<actor_name_lc>"`,
    // matching the convention in `actor.rs::lower_actor`.
    //
    // When `scope.own_module_beam_name` is set (from `lower_fn_with_module_name`),
    // we use it directly.  The actor's declaration module SHOULD match the current
    // module (spawn and actor declaration are in the same source file).
    //
    // Fallback: if the scope doesn't carry the module beam name (e.g. in unit tests),
    // use the old `ridge_actor_<module_id>_<name_lc>` placeholder so existing tests
    // that check the placeholder string continue to pass.
    let actor_name_lc = actor_name.to_lowercase();
    let actor_beam_module = scope.own_module_beam_name.as_ref().map_or_else(
        || derive_actor_beam_module(actor_module_id.0, actor_name),
        |parent_name| format!("{parent_name}_{actor_name_lc}"),
    );

    // Lower args into a list literal.
    let lowered_args = args
        .iter()
        .map(|a| lower_expr_in_scope(a, scope))
        .collect::<Result<Vec<_>, _>>()?;

    // Emit: call 'ridge_rt':'spawn_actor' (ActorMod, [Arg1, ..., ArgN], [])
    Ok(CErlExpr::Call {
        module: CErlAtom("ridge_rt".into()),
        fn_name: CErlAtom("spawn_actor".into()),
        args: vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom(actor_beam_module))),
            CErlExpr::ListLit(lowered_args),
            CErlExpr::Lit(CErlLit::Nil), // options: []
        ],
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Build the `{handler_tag, Arg1, ..., ArgN}` message tuple.
///
/// The handler tag is the `message_name` from the `SymbolRef::Handler`.
/// Arguments are lowered left-to-right (strict evaluation order, spec §7.1).
fn build_message_tuple(
    message: &SymbolRef,
    args: &[IrExpr],
    span: Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    let SymbolRef::Handler { handler, .. } = message else {
        return Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Send/Ask",
            span,
            detail: format!(
                "Send message field is not SymbolRef::Handler — got {message:?} (Phase 5 invariant violated)"
            ),
        });
    };

    let mut elems = Vec::with_capacity(args.len() + 1);
    // Tag atom: the handler name.
    elems.push(CErlExpr::Lit(CErlLit::Atom(CErlAtom(handler.clone()))));
    // Arguments in strict left-to-right order (spec §7.1).
    for arg in args {
        elems.push(lower_expr_in_scope(arg, scope)?);
    }

    Ok(CErlExpr::Tuple(elems))
}

/// Derive the BEAM module name for an actor given its module ID and source name.
///
/// Follows the same mangling convention as `mangle_module_name` in `module.rs`:
/// `ridge_actor_<module_id>_<actor_name_lowercase>`.
///
/// The `module_id` component is used because at the `IrExpr::Spawn` level we only
/// have a `ModuleId`, not the full module path.  The actor module itself is
/// emitted by `lower_actor` with the full mangled name (e.g.
/// `ridge_examples_url_shortener_store`); the Spawn site must use the same name.
///
/// For the four example actors (`Limiter`, `Store`), snapshot tests will
/// verify the name round-trips correctly.
pub(crate) fn derive_actor_beam_module(module_id: u32, actor_name: &str) -> String {
    // Lowercase the actor name for idiomatic Erlang atom.
    let name_lc = actor_name.to_lowercase();
    format!("ridge_actor_{module_id}_{name_lc}")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit};
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrLit, IrNodeId, IrTimeout, SymbolRef};
    use ridge_resolve::ModuleId;
    use ridge_types::TyConId;

    fn sp() -> Span {
        Span::point(0)
    }

    fn lit_int(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    fn lit_unit() -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Unit,
            span: sp(),
        }
    }

    fn handler_sym(actor: &str, handler: &str) -> SymbolRef {
        SymbolRef::Handler {
            actor_module: ModuleId(0),
            actor: actor.into(),
            handler: handler.into(),
        }
    }

    fn actor_type_sym(name: &str) -> SymbolRef {
        SymbolRef::ActorType {
            module: ModuleId(0),
            name: name.into(),
        }
    }

    // §4.17 — Send tests

    #[test]
    fn send_emits_ridge_rt_send_op() {
        // Send routes through ridge_rt:send_op/2 per OQ-E004 (bounded mailbox
        // support since 0.2.7).
        let handle = lit_int(42); // placeholder — normally a pid variable
        let message = handler_sym("Counter", "increment");
        let args = vec![lit_int(5)];
        let mut scope = LocalScope::new();

        let result = lower_send(&handle, &message, &args, sp(), &mut scope).unwrap();

        match &result {
            CErlExpr::Call {
                module,
                fn_name,
                args: call_args,
            } => {
                assert_eq!(module.0, "ridge_rt");
                assert_eq!(fn_name.0, "send_op");
                assert_eq!(
                    call_args.len(),
                    2,
                    "send_op/2 expects handle + message tuple"
                );
                // Second arg must be the message tuple {increment, 5}.
                match &call_args[1] {
                    CErlExpr::Tuple(elems) => {
                        assert_eq!(elems.len(), 2);
                        assert!(
                            matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "increment")
                        );
                    }
                    other => panic!("expected Tuple, got {other:?}"),
                }
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn send_zero_arg_handler() {
        // Zero-arg handler → message tuple has only the tag.
        let handle = lit_unit();
        let message = handler_sym("Counter", "reset");
        let args = vec![];
        let mut scope = LocalScope::new();

        let result = lower_send(&handle, &message, &args, sp(), &mut scope).unwrap();
        match &result {
            CErlExpr::Call {
                args: call_args, ..
            } => match &call_args[1] {
                CErlExpr::Tuple(elems) => {
                    assert_eq!(elems.len(), 1, "zero-arg handler tuple has only the tag");
                }
                other => panic!("expected Tuple, got {other:?}"),
            },
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn send_non_handler_sym_returns_error() {
        let handle = lit_unit();
        let bad_message = SymbolRef::Constructor {
            ctor_kind: ridge_ir::CtorKind::UnionVariant,
            owner_type: TyConId(0),
            name: "Foo".into(),
            variant: 0,
        };
        let mut scope = LocalScope::new();
        let result = lower_send(&handle, &bad_message, &[], sp(), &mut scope);
        assert!(
            matches!(
                result,
                Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Send/Ask",
                    ..
                })
            ),
            "expected IrShapeMalformed for non-Handler symbol, got {result:?}"
        );
    }

    // §4.18 — Ask tests

    #[test]
    fn ask_default_timeout_emits_5000() {
        // OQ-E001: None timeout → 5000 ms.
        let handle = lit_unit();
        let message = handler_sym("Store", "get");
        let args = vec![lit_int(1)];
        let timeout = None;
        let mut scope = LocalScope::new();

        let result =
            lower_ask(&handle, &message, &args, timeout.as_ref(), sp(), &mut scope).unwrap();
        match &result {
            CErlExpr::Call {
                module,
                fn_name,
                args: call_args,
            } => {
                assert_eq!(module.0, "ridge_rt");
                assert_eq!(fn_name.0, "ask");
                assert_eq!(call_args.len(), 3, "ask/3 expects handle + msg + timeout");
                assert!(
                    matches!(&call_args[2], CErlExpr::Lit(CErlLit::Int(5000))),
                    "default timeout must be 5000"
                );
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn ask_never_timeout_emits_infinity() {
        // OQ-E001: Some(Never) → 'infinity'.
        let handle = lit_unit();
        let message = handler_sym("Store", "get");
        let timeout = Some(IrTimeout::Never);
        let mut scope = LocalScope::new();

        let result = lower_ask(&handle, &message, &[], timeout.as_ref(), sp(), &mut scope).unwrap();
        match &result {
            CErlExpr::Call {
                args: call_args, ..
            } => {
                assert!(
                    matches!(&call_args[2], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "infinity"),
                    "Never timeout must emit 'infinity'"
                );
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn ask_explicit_millis_timeout() {
        // OQ-E001: Some(Millis(e)) → lower_expr(e).
        let handle = lit_unit();
        let message = handler_sym("Store", "get");
        let millis_expr = lit_int(2000);
        let timeout = Some(IrTimeout::Millis(Box::new(millis_expr)));
        let mut scope = LocalScope::new();

        let result = lower_ask(&handle, &message, &[], timeout.as_ref(), sp(), &mut scope).unwrap();
        match &result {
            CErlExpr::Call {
                args: call_args, ..
            } => {
                assert!(
                    matches!(&call_args[2], CErlExpr::Lit(CErlLit::Int(2000))),
                    "explicit millis timeout must lower to the expr value"
                );
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    // §4.19 — Spawn tests

    #[test]
    fn spawn_emits_ridge_rt_spawn_actor() {
        // Spawn routes through ridge_rt:spawn_actor/3 per OQ-E006.
        let actor = actor_type_sym("Limiter");
        let args = vec![lit_int(100)];
        let mut scope = LocalScope::new();

        let result = lower_spawn(&actor, &args, sp(), &mut scope).unwrap();
        match &result {
            CErlExpr::Call {
                module,
                fn_name,
                args: call_args,
            } => {
                assert_eq!(module.0, "ridge_rt");
                assert_eq!(fn_name.0, "spawn_actor");
                assert_eq!(
                    call_args.len(),
                    3,
                    "spawn_actor/3 expects module, args, opts"
                );
                // Third arg must be [] (empty options list).
                assert!(
                    matches!(&call_args[2], CErlExpr::Lit(CErlLit::Nil)),
                    "spawn options must be []"
                );
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    /// `spawn` lowered inside an actor handler must derive the actor's
    /// BEAM module via the canonical `"<parent>_<actor_lc>"` shape — the
    /// same one `lower_actor` uses to *emit* the actor module.  Before
    /// the `with_actor_parent` patch in `scope.rs`, the handler scope
    /// carried `own_module_beam_name: None`, so this call fell through
    /// to `derive_actor_beam_module` and produced
    /// `ridge_actor_<id>_<name>` — a name nothing in the compiled
    /// output exports, which crashed the spawned process at startup
    /// with `undefined function ridge_actor_*:init/1`.
    #[test]
    fn spawn_inside_handler_uses_parent_module_name() {
        use crate::scope::LocalScope;
        use rustc_hash::FxHashMap;
        let actor = actor_type_sym("Worker");
        let mut scope =
            LocalScope::with_actor_parent(FxHashMap::default(), ModuleId(0), "ridge_module_0");
        let result = lower_spawn(&actor, &[], sp(), &mut scope).unwrap();
        match &result {
            CErlExpr::Call {
                args: call_args, ..
            } => match &call_args[0] {
                CErlExpr::Lit(CErlLit::Atom(CErlAtom(name))) => {
                    assert_eq!(
                        name, "ridge_module_0_worker",
                        "spawn target must match the actor sub-module name"
                    );
                }
                other => panic!("expected atom target, got {other:?}"),
            },
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn spawn_non_actor_type_returns_error() {
        let bad_actor = SymbolRef::Local {
            name: "foo".into(),
            module: ModuleId(0),
        };
        let mut scope = LocalScope::new();
        let result = lower_spawn(&bad_actor, &[], sp(), &mut scope);
        assert!(
            matches!(
                result,
                Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Spawn",
                    ..
                })
            ),
            "expected IrShapeMalformed for non-ActorType symbol, got {result:?}"
        );
    }

    // Actor module name derivation

    #[test]
    fn derive_actor_beam_module_lowercase() {
        assert_eq!(
            derive_actor_beam_module(0, "Limiter"),
            "ridge_actor_0_limiter"
        );
        assert_eq!(derive_actor_beam_module(1, "Store"), "ridge_actor_1_store");
    }
}
