//! Actor send/ask/spawn type inference and encapsulation check (T15).
//!
//! Implements §3.7 (Send/Ask handler-name validation) and §4.15 (actor
//! capability encapsulation) from the Phase-4 plan.
//!
//! # Entry points
//!
//! - [`infer_send`] — type-infer `handle ! message`
//! - [`infer_ask`]  — type-infer `handle ?> message args`
//! - [`infer_spawn`] — type-infer `spawn Actor args`
//! - [`check_actor_encapsulation`] — verify per §4.15 rule 2 that the actor's
//!   declared cap set equals the union of its handlers' declared caps.
//!
//! # Capability Model B (§8.4, D018)
//!
//! - `Send` contributes PURE to the caller's cap set.
//! - `Ask`  contributes `{time}` to the caller's cap set.
//! - `Spawn` contributes `{spawn}` to the caller's cap set.
//!
//! The handler's own caps NEVER flow into the caller (Model B encapsulation).
//! T13 (`caps_infer.rs`) implements this; T15 verifies it is not regressed.

use ridge_ast::{AskTimeout, Expr, Ident, Span};
use ridge_types::{ActorSchema, BuiltinTyCons, CapabilitySet, TyConId, TyConKind, Type};

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::infer::infer_expr;
use crate::render::emit_internal;
use crate::unify::unify;

// ── Send ──────────────────────────────────────────────────────────────────────

/// Type-infer `handle ! message` (§3.7 rule 1-2 + §4.15 rule 1).
///
/// 1. Infers `handle`'s type — must resolve to `Type::Con(actor_id, _)` where
///    the `actor_id`'s kind is `TyConKind::Actor(schema)`. Otherwise → T020.
/// 2. Extracts the handler name and args from `message`:
///    - `Expr::Call { callee: Expr::Ident(name), args }` — name + args
///    - `Expr::Ident(name)` — name + zero args
///    - Anything else → T999 (parser invariant violation).
/// 3. Looks up handler name in `actor_schema.handlers`. Missing → T015 with
///    did-you-mean from `ridge_resolve::suggest::suggest`.
/// 4. Pairwise-unifies args against `handler.params`. Arity mismatch → T003.
/// 5. Returns `Type::Con(b.unit, [])` (Send is fire-and-forget).
/// 6. Capability contribution: PURE (T13 already handles, T15 does not re-emit).
pub fn infer_send(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    handle: &Expr,
    message: &Expr,
    span: Span,
    arena: &ridge_types::TyConArena,
) -> Type {
    // Step 1 — infer handle type and require it to be an actor.
    let handle_ty = infer_expr(ctx, b, handle);

    // Absorb: if the handle type is a free type variable (e.g. a HOF callback
    // param constrained after body inference), return Unit silently.
    // T020 fires only for concrete non-actor types.
    if matches!(ctx.deep_resolve(&handle_ty), Type::Var(_) | Type::Error) {
        return Type::Con(b.unit, vec![]);
    }

    let Ok((actor_id, actor_schema)) = resolve_actor_type(ctx, arena, &handle_ty) else {
        // Not an actor handle.
        let found_ty = format!("{handle_ty:?}");
        ctx.errors
            .push(TypeError::SendOnNonActor { found_ty, span });
        return Type::Error;
    };

    // Step 2 — extract handler name and args from the message Expr.
    let Some((handler_name, msg_args)) = extract_handler_call(message) else {
        return emit_internal(
            ctx,
            "Send message is neither Ident nor Call — parser invariant violation",
            span,
        );
    };

    // Step 3 — look up handler name in actor schema.
    let Some(handler) = actor_schema
        .handlers
        .iter()
        .find(|h| h.name == handler_name)
    else {
        let suggestions = ridge_resolve::suggest::suggest(
            &handler_name,
            actor_schema.handlers.iter().map(|h| h.name.clone()),
        );
        let actor_name = arena.get(actor_id).name.clone();
        ctx.errors.push(TypeError::UnknownActorHandler {
            actor: actor_name,
            handler: handler_name,
            suggestions,
            span,
        });
        return Type::Error;
    };

    // Step 4 — pairwise unify args against handler.params.
    let actor_name = arena.get(actor_id).name.clone();
    check_handler_args(
        ctx,
        b,
        &actor_name,
        &handler.name,
        &handler.params,
        msg_args,
        span,
    );

    // Step 5 — Send returns Unit.
    Type::Con(b.unit, vec![])
}

// ── Ask ───────────────────────────────────────────────────────────────────────

/// Type-infer `handle ?> message args [timeout <ms|never>]`
/// (§3.7 rule 1, 3 + §4.15 rule 1; timeout type-check added by Phase 6 T0, OQ-E001).
///
/// Steps 1-3 identical to `infer_send`.
/// Step 4: pairwise-unify args against `handler.params`.
/// Step 5: if `timeout == Some(Millis(e))`, constrain `e: Int` (T026).
///         `Never` carries no expression — no new constraint.
/// Step 6: returns `handler.ret` (the handler's declared return type).
/// Capability contribution: `{time}` (T13 already handles).
#[allow(clippy::too_many_arguments)] // T0: timeout added to the protocol-level Ask inference
pub fn infer_ask(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    handle: &Expr,
    message: &Ident,
    args: &[Expr],
    timeout: Option<&AskTimeout>,
    span: Span,
    arena: &ridge_types::TyConArena,
) -> Type {
    // Step 1 — infer handle type.
    let handle_ty = infer_expr(ctx, b, handle);

    // Absorb: free type variable handle — return a fresh var silently.
    // T021 fires only for concrete non-actor types.
    if matches!(ctx.deep_resolve(&handle_ty), Type::Var(_) | Type::Error) {
        return Type::Var(ctx.fresh_tyvid());
    }

    let Ok((actor_id, actor_schema)) = resolve_actor_type(ctx, arena, &handle_ty) else {
        let found_ty = format!("{handle_ty:?}");
        ctx.errors.push(TypeError::AskOnNonActor { found_ty, span });
        return Type::Error;
    };

    let handler_name = message.text.clone();

    // Step 3 — look up handler name.
    let Some(handler) = actor_schema
        .handlers
        .iter()
        .find(|h| h.name == handler_name)
    else {
        let suggestions = ridge_resolve::suggest::suggest(
            &handler_name,
            actor_schema.handlers.iter().map(|h| h.name.clone()),
        );
        let actor_name = arena.get(actor_id).name.clone();
        ctx.errors.push(TypeError::UnknownActorHandler {
            actor: actor_name,
            handler: handler_name,
            suggestions,
            span,
        });
        return Type::Error;
    };

    let ret_ty = handler.ret.clone();
    let actor_name = arena.get(actor_id).name.clone();

    // Step 4 — pairwise unify args.
    check_handler_args(
        ctx,
        b,
        &actor_name,
        &handler.name,
        &handler.params,
        args,
        span,
    );

    // Step 5 — type-check optional timeout (Phase 6 T0, OQ-E001).
    //
    // `timeout never` carries no expression — no type constraint.
    // `timeout <expr>` requires `expr: Int` (T026 AskTimeoutNotInt).
    // The inner expression is a regular sub-expression that gets inferred
    // and entered into the node_types side-table via the usual infer path.
    if let Some(AskTimeout::Millis(ms_expr)) = timeout {
        let ms_ty = infer_expr(ctx, b, ms_expr);
        let int_ty = Type::Con(b.int, vec![]);
        // T026: the timeout expression must unify with Int.
        // `unify` returns `Err(TypeError)` on failure; we push T026 in that case.
        // Code T026 is allocated here (see crate::error — T001..T025 were prior).
        if unify(ctx, &ms_ty, &int_ty).is_err() {
            let found_ty = format!("{ms_ty:?}");
            ctx.errors.push(TypeError::AskTimeoutNotInt {
                found: found_ty,
                span: ms_expr.span(),
            });
        }
    }

    // Step 6 — return handler's ret type.
    ret_ty
}

// ── Spawn ─────────────────────────────────────────────────────────────────────

/// Type-infer `spawn Actor args` (§3.7 rule 4 + §4.15 rule 1).
///
/// 1. Looks up `actor_name` (an `Ident`) in `arena` by name.
///    If not found / not an actor → T999 (resolver should have caught it).
/// 2. If `actor_schema.init_params == None`: `args` must be empty; else T025.
/// 3. If `init_params == Some(params)`: `args.len() == params.len()`; else T025.
///    Pairwise-unify each arg with its declared init param type.
/// 4. Returns `Type::Con(actor_id, [])`.
///
/// Capability contribution: `{spawn}` (T13 handles).
pub fn infer_spawn(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    actor_ident: &Ident,
    args: &[Expr],
    span: Span,
    arena: &ridge_types::TyConArena,
) -> Type {
    // Step 1 — resolve actor name in arena.
    let actor_id_opt = arena
        .all()
        .iter()
        .find(|d| matches!(&d.kind, TyConKind::Actor(_)) && d.name == actor_ident.text)
        .map(|d| d.id);

    let Some(actor_id) = actor_id_opt else {
        return emit_internal(
            ctx,
            format!(
                "spawn: actor '{}' not found in arena — resolver should have caught this",
                actor_ident.text
            ),
            span,
        );
    };

    let TyConKind::Actor(actor_schema_ref) = &arena.get(actor_id).kind else {
        return emit_internal(
            ctx,
            format!("spawn: '{}' is not an actor type", actor_ident.text),
            span,
        );
    };
    let actor_schema = actor_schema_ref.clone();

    // Steps 2 & 3 — check init arity and unify arg types.
    match &actor_schema.init_params {
        None => {
            if !args.is_empty() {
                ctx.errors.push(TypeError::SpawnArityMismatch {
                    actor: actor_ident.text.clone(),
                    expected: 0,
                    found: args.len(),
                    span,
                });
                return Type::Error;
            }
        }
        Some(params) => {
            if args.len() != params.len() {
                ctx.errors.push(TypeError::SpawnArityMismatch {
                    actor: actor_ident.text.clone(),
                    expected: params.len(),
                    found: args.len(),
                    span,
                });
                return Type::Error;
            }
            for (arg, param_ty) in args.iter().zip(params.iter()) {
                let arg_ty = infer_expr(ctx, b, arg);
                if let Err(e) = unify(ctx, &arg_ty, param_ty) {
                    // Attach span to the unification error.
                    let e_spanned = attach_span(e, span);
                    ctx.errors.push(e_spanned);
                }
            }
        }
    }

    // Step 4 — return `Handle<actor>` = `Type::Con(b.handle, [Con(actor_id, [])])`.
    // D061: `spawn ActorName args` produces a `Handle(ActorTyCon)`.
    Type::Con(b.handle, vec![Type::Con(actor_id, vec![])])
}

// ── Actor encapsulation check ─────────────────────────────────────────────────

/// Per §4.15 rule 2: the actor's declared cap set must equal the union of its
/// handlers' declared caps.
///
/// Fires `T019 ActorCapabilityLeak` when the actor's `init` block declares
/// capabilities that fall outside the union of the actor's handler caps.
///
/// In 0.1.0, actors have no explicit cap annotation in the AST; the actor's
/// effective capability set is computed as the union of all handler caps.
/// Handlers themselves are always within this union by construction, so T019
/// is only reachable through the `init` block — e.g., an `init` that calls
/// `Io.println` (needs `{io}`) while no handler declares `{io}`.
///
/// This is a defensive check.  The spec says "should not fire today" for
/// handler caps; this implementation fires on `init` cap mismatches (D018 Model
/// B: actor's boundary is the union of handler caps).
///
/// # Arguments
///
/// - `actor_name` — the actor's type name (for the diagnostic).
/// - `actor_caps` — the effective capability set of the actor (union of handler caps).
/// - `schema` — the actor's schema containing handler and init definitions.
/// - `handler_spans` — optional per-handler spans for diagnostics; if
///   `handler_spans[i]` is `None`, `fallback_span` is used.
/// - `fallback_span` — used when no per-handler span is available.
#[must_use]
pub fn check_actor_encapsulation(
    actor_name: &str,
    actor_caps: CapabilitySet,
    schema: &ActorSchema,
    handler_spans: &[Option<Span>],
    fallback_span: Span,
) -> Vec<TypeError> {
    let mut errors = Vec::new();

    // Check each handler: caps present in handler.caps but absent from actor_caps.
    // With actor_caps = union(handler_caps), this is always empty for handlers.
    // Kept for future-proofing (if explicit actor-level cap annotations are added).
    for (i, handler) in schema.handlers.iter().enumerate() {
        let leaking = handler.caps.difference(&actor_caps);
        if !leaking.is_pure() {
            let hspan = handler_spans
                .get(i)
                .and_then(|s| *s)
                .unwrap_or(fallback_span);
            errors.push(TypeError::ActorCapabilityLeak {
                actor: actor_name.to_string(),
                handler: handler.name.clone(),
                leaking_caps: leaking,
                span: hspan,
            });
        }
    }

    // Check the init block: init_caps must be ⊆ actor_caps (union of handler caps).
    // T019 fires if the init block declares a capability not present in any handler.
    // This catches the case where an init block uses IO/FS/etc. while no handler
    // ever uses those capabilities — a genuine capability leak at the actor boundary.
    let init_leaking = schema.init_caps.difference(&actor_caps);
    if !init_leaking.is_pure() {
        errors.push(TypeError::ActorCapabilityLeak {
            actor: actor_name.to_string(),
            handler: "init".to_string(),
            leaking_caps: init_leaking,
            span: fallback_span,
        });
    }

    errors
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Resolves a type to `(TyConId, ActorSchema)` if it is a concrete actor type.
///
/// Accepts:
/// - `Type::Con(id, _)` where `arena.get(id).kind == TyConKind::Actor(_)`.
///
/// Returns `Err(())` for anything else (type variable, non-actor Con, etc.).
fn resolve_actor_type(
    ctx: &mut InferCtx,
    arena: &ridge_types::TyConArena,
    ty: &Type,
) -> Result<(TyConId, ActorSchema), ()> {
    let resolved = ctx.shallow_resolve(ty);
    match resolved {
        // Direct actor type: `Counter` (bare actor constructor, e.g. in spawn).
        Type::Con(id, args) => {
            let decl = arena.get(id);
            match &decl.kind {
                TyConKind::Actor(schema) => Ok((id, schema.clone())),
                // Handle<X> — unwrap the first type argument and resolve as actor.
                // `Handle Counter` = Con(handle_id, [Con(counter_id, [])]).
                TyConKind::Builtin => {
                    if let Some(inner) = args.first() {
                        let inner_resolved = ctx.shallow_resolve(inner);
                        if let Type::Con(inner_id, _) = inner_resolved {
                            let inner_decl = arena.get(inner_id);
                            if let TyConKind::Actor(schema) = &inner_decl.kind {
                                return Ok((inner_id, schema.clone()));
                            }
                        }
                    }
                    Err(())
                }
                _ => Err(()),
            }
        }
        _ => Err(()),
    }
}

/// Extracts `(handler_name, args)` from a Send message expression.
///
/// The parser represents `handle ! foo arg1 arg2` as:
/// - `Expr::Call { callee: Expr::Ident("foo"), args: [arg1, arg2] }`
///
/// And `handle ! foo` (no args) as:
/// - `Expr::Ident("foo")`
///
/// Returns `None` for any other shape (T999 internal error at call site).
fn extract_handler_call(message: &Expr) -> Option<(String, &[Expr])> {
    match message {
        Expr::Ident(id) => Some((id.text.clone(), &[])),
        Expr::Call { callee, args, .. } => {
            if let Expr::Ident(id) = callee.as_ref() {
                Some((id.text.clone(), args.as_slice()))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Pairwise-unifies `args` against `handler_params`, emitting:
/// - `T003 ArityMismatch` if lengths differ.
/// - `T001 TypeMismatch` (via `unify`) if individual types differ.
///
/// A single `Unit` literal argument against a zero-parameter handler is
/// accepted, mirroring the surface symmetry between the handler decl
/// `on name ()` and the call site `handle ?> name ()`: both forms read the
/// `()` as "no payload", not as a unit-typed argument.  The normalisation
/// happens before the arity check so the call still type-checks against a
/// 0-arity handler, and `infer_expr` is still walked on the unit literal so
/// node_types stays populated for it.
fn check_handler_args(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    actor_name: &str,
    handler_name: &str,
    handler_params: &[Type],
    args: &[Expr],
    span: Span,
) {
    let normalised_args: &[Expr] =
        if handler_params.is_empty() && args.len() == 1 && matches!(&args[0], Expr::Unit(_)) {
            let _ = infer_expr(ctx, b, &args[0]);
            &[]
        } else {
            args
        };

    if normalised_args.len() != handler_params.len() {
        ctx.errors.push(TypeError::ArityMismatch {
            callee: format!("{actor_name}.{handler_name}"),
            expected: handler_params.len(),
            found: normalised_args.len(),
            span,
            hint: None,
        });
        return;
    }

    for (arg, param_ty) in normalised_args.iter().zip(handler_params.iter()) {
        let arg_ty = infer_expr(ctx, b, arg);
        if let Err(e) = unify(ctx, &arg_ty, param_ty) {
            let e_spanned = attach_span(e, span);
            ctx.errors.push(e_spanned);
        }
    }
}

/// Attaches `span` to a `TypeError` that carries a dummy span.
///
/// Only replaces spans that are `Span::point(0)` (the canonical dummy span
/// used by `unify`). If the error already has a non-dummy span it is returned
/// unchanged.
fn attach_span(e: TypeError, span: Span) -> TypeError {
    use ridge_ast::Span;
    let dummy = Span::point(0);
    match e {
        TypeError::TypeMismatch {
            expected,
            found,
            span: s,
        } if s == dummy => TypeError::TypeMismatch {
            expected,
            found,
            span,
        },
        TypeError::OccursCheck { var, ty, span: s } if s == dummy => {
            TypeError::OccursCheck { var, ty, span }
        }
        other => other,
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Block, Capability, Expr, Ident, Literal, Span};
    use ridge_types::{
        ActorSchema, BuiltinTyCons, CapabilitySet, HandlerSchema, RecordField, TyConArena,
        TyConDecl, TyConKind,
    };

    // ── Test helpers ─────────────────────────────────────────────────────────

    fn ds() -> Span {
        Span::point(0)
    }

    fn id(t: &str) -> Ident {
        Ident {
            text: t.to_string(),
            span: ds(),
        }
    }

    fn make_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    fn int_lit(n: i64) -> Expr {
        Expr::Literal(Literal::IntDec {
            raw: n.to_string(),
            span: ds(),
        })
    }

    fn text_lit(s: &str) -> Expr {
        Expr::Literal(Literal::Text {
            raw: format!("\"{s}\""),
            span: ds(),
        })
    }

    /// Register a `Counter` actor in the arena:
    ///
    /// ```text
    /// actor Counter {
    ///     state: count = 0
    ///     on increment(n: Int) -> Unit
    ///     on getCount() -> Int
    /// }
    /// ```
    ///
    /// Returns `(actor_id, b)`.
    fn register_counter(arena: &mut TyConArena, b: &BuiltinTyCons) -> TyConId {
        let schema = ActorSchema {
            state_fields: vec![RecordField {
                name: "count".to_string(),
                ty: Type::Con(b.int, vec![]),
            }],
            init_params: Some(vec![Type::Con(b.int, vec![])]),
            init_caps: CapabilitySet::PURE,
            handlers: vec![
                HandlerSchema {
                    name: "increment".to_string(),
                    params: vec![Type::Con(b.int, vec![])],
                    ret: Type::Con(b.unit, vec![]),
                    caps: CapabilitySet::PURE,
                },
                HandlerSchema {
                    name: "getCount".to_string(),
                    params: vec![],
                    ret: Type::Con(b.int, vec![]),
                    caps: CapabilitySet::PURE,
                },
            ],
        };
        arena.intern(TyConDecl {
            id: ridge_types::TyConId(0), // overwritten by intern
            name: "Counter".to_string(),
            arity: 0,
            kind: TyConKind::Actor(schema),
            def_span: None,
        })
    }

    /// Register a `Logger` actor with NO init:
    ///
    /// ```text
    /// actor Logger {
    ///     on log(msg: Text) -> Unit
    /// }
    /// ```
    fn register_logger(arena: &mut TyConArena, b: &BuiltinTyCons) -> TyConId {
        let schema = ActorSchema {
            state_fields: vec![],
            init_params: None,
            init_caps: CapabilitySet::PURE,
            handlers: vec![HandlerSchema {
                name: "log".to_string(),
                params: vec![Type::Con(b.text, vec![])],
                ret: Type::Con(b.unit, vec![]),
                caps: CapabilitySet::PURE,
            }],
        };
        arena.intern(TyConDecl {
            id: ridge_types::TyConId(0),
            name: "Logger".to_string(),
            arity: 0,
            kind: TyConKind::Actor(schema),
            def_span: None,
        })
    }

    /// Bind `name` → `Type::Con(actor_id, [])` as a monotype in ctx.
    fn bind_actor_handle(ctx: &mut InferCtx, name: &str, actor_id: TyConId) {
        use ridge_types::Scheme;
        ctx.env
            .bind(name.to_string(), Scheme::mono(Type::Con(actor_id, vec![])));
    }

    // ── T15-1: send_known_handler_ok ─────────────────────────────────────────

    #[test]
    fn send_known_handler_ok() {
        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        // counter ! increment 1
        let handle = Expr::Ident(id("counter"));
        let message = Expr::Call {
            callee: Box::new(Expr::Ident(id("increment"))),
            args: vec![int_lit(1)],
            span: ds(),
        };

        let ty = infer_send(&mut ctx, &b, &handle, &message, ds(), &arena);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors, got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.unit),
            "Send must return Unit, got {ty:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T15-2: send_unknown_handler_T015 ─────────────────────────────────────

    #[test]
    fn send_unknown_handler_t015() {
        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        // counter ! incremento   (typo of "increment")
        let handle = Expr::Ident(id("counter"));
        let message = Expr::Ident(id("incremento"));

        let ty = infer_send(&mut ctx, &b, &handle, &message, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected exactly one error");
        assert_eq!(ctx.errors[0].code(), "T015");
        if let TypeError::UnknownActorHandler {
            actor,
            handler,
            suggestions,
            ..
        } = &ctx.errors[0]
        {
            assert_eq!(actor, "Counter");
            assert_eq!(handler, "incremento");
            assert!(
                suggestions.contains(&"increment".to_string()),
                "expected 'increment' in suggestions, got {suggestions:?}"
            );
        }
        assert!(matches!(ty, Type::Error));
        ctx.env.pop_frame();
    }

    // ── T15-3: send_handler_arity_mismatch_T003 ───────────────────────────────

    #[test]
    fn send_handler_arity_mismatch_t003() {
        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        // counter ! increment 1 2   (increment takes only 1 param)
        let handle = Expr::Ident(id("counter"));
        let message = Expr::Call {
            callee: Box::new(Expr::Ident(id("increment"))),
            args: vec![int_lit(1), int_lit(2)],
            span: ds(),
        };

        infer_send(&mut ctx, &b, &handle, &message, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected exactly one error");
        assert_eq!(ctx.errors[0].code(), "T003");
        ctx.env.pop_frame();
    }

    // ── T15-4: send_on_non_actor_T020 ────────────────────────────────────────

    #[test]
    fn send_on_non_actor_t020() {
        let (arena, b) = make_builtins();

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // 42 ! foo
        let handle = int_lit(42);
        let message = Expr::Ident(id("foo"));

        let ty = infer_send(&mut ctx, &b, &handle, &message, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected T020");
        assert_eq!(ctx.errors[0].code(), "T020");
        assert!(matches!(ty, Type::Error));
        ctx.env.pop_frame();
    }

    // ── T15-5: ask_known_handler_ok ──────────────────────────────────────────

    #[test]
    fn ask_known_handler_ok() {
        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        // counter ?> getCount   (returns Int)
        let handle = Expr::Ident(id("counter"));
        let message = id("getCount");

        let ty = infer_ask(&mut ctx, &b, &handle, &message, &[], None, ds(), &arena);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors, got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "Ask on getCount must return Int, got {ty:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T15-6: ask_unknown_handler_T015 ──────────────────────────────────────

    #[test]
    fn ask_unknown_handler_t015() {
        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        // counter ?> getKount   (typo of "getCount")
        let handle = Expr::Ident(id("counter"));
        let message = id("getKount");

        let ty = infer_ask(&mut ctx, &b, &handle, &message, &[], None, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected T015");
        assert_eq!(ctx.errors[0].code(), "T015");
        if let TypeError::UnknownActorHandler {
            handler,
            suggestions,
            ..
        } = &ctx.errors[0]
        {
            assert_eq!(handler, "getKount");
            assert!(
                suggestions.contains(&"getCount".to_string()),
                "expected 'getCount' in suggestions, got {suggestions:?}"
            );
        }
        assert!(matches!(ty, Type::Error));
        ctx.env.pop_frame();
    }

    // ── T15-7: ask_on_non_actor_T021 ─────────────────────────────────────────

    #[test]
    fn ask_on_non_actor_t021() {
        let (arena, b) = make_builtins();

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // 42 ?> foo
        let handle = int_lit(42);
        let message = id("foo");

        let ty = infer_ask(&mut ctx, &b, &handle, &message, &[], None, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected T021");
        assert_eq!(ctx.errors[0].code(), "T021");
        assert!(matches!(ty, Type::Error));
        ctx.env.pop_frame();
    }

    // ── T15-8: spawn_known_actor_ok ──────────────────────────────────────────

    #[test]
    fn spawn_known_actor_ok() {
        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // spawn Counter 0   (Counter has init: Int)
        let actor = id("Counter");
        let args = vec![int_lit(0)];

        let ty = infer_spawn(&mut ctx, &b, &actor, &args, ds(), &arena);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors, got {:?}",
            ctx.errors
        );
        // spawn returns Handle<Actor> = Con(b.handle, [Con(actor_id, [])]).
        assert!(
            matches!(&ty, Type::Con(id, args) if *id == b.handle
                && matches!(args.first(), Some(Type::Con(inner, _)) if *inner == counter_id)),
            "spawn Counter must return Handle Counter, got {ty:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T15-9: spawn_no_init_args_ok ─────────────────────────────────────────

    #[test]
    fn spawn_no_init_args_ok() {
        let (mut arena, b) = make_builtins();
        let logger_id = register_logger(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // spawn Logger   (Logger has no init)
        let actor = id("Logger");

        let ty = infer_spawn(&mut ctx, &b, &actor, &[], ds(), &arena);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors, got {:?}",
            ctx.errors
        );
        // spawn returns Handle<Actor>.
        assert!(
            matches!(&ty, Type::Con(id, args) if *id == b.handle
                && matches!(args.first(), Some(Type::Con(inner, _)) if *inner == logger_id)),
            "spawn Logger must return Handle Logger, got {ty:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T15-10: spawn_no_init_extra_args_T025 ────────────────────────────────

    #[test]
    fn spawn_no_init_extra_args_t025() {
        let (mut arena, b) = make_builtins();
        let _logger_id = register_logger(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // spawn Logger 1 2   (Logger has no init, but we pass 2 args)
        let actor = id("Logger");
        let args = vec![int_lit(1), int_lit(2)];

        let ty = infer_spawn(&mut ctx, &b, &actor, &args, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected T025");
        assert_eq!(ctx.errors[0].code(), "T025");
        if let TypeError::SpawnArityMismatch {
            actor: a,
            expected,
            found,
            ..
        } = &ctx.errors[0]
        {
            assert_eq!(a, "Logger");
            assert_eq!(*expected, 0);
            assert_eq!(*found, 2);
        }
        assert!(matches!(ty, Type::Error));
        ctx.env.pop_frame();
    }

    // ── T15-11: spawn_init_arity_mismatch_T025 ───────────────────────────────

    #[test]
    fn spawn_init_arity_mismatch_t025() {
        let (mut arena, b) = make_builtins();
        let _counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // spawn Counter   (Counter requires Int arg)
        let actor = id("Counter");

        let ty = infer_spawn(&mut ctx, &b, &actor, &[], ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected T025");
        assert_eq!(ctx.errors[0].code(), "T025");
        if let TypeError::SpawnArityMismatch {
            actor: a,
            expected,
            found,
            ..
        } = &ctx.errors[0]
        {
            assert_eq!(a, "Counter");
            assert_eq!(*expected, 1);
            assert_eq!(*found, 0);
        }
        assert!(matches!(ty, Type::Error));
        ctx.env.pop_frame();
    }

    // ── T15-12: spawn_init_type_mismatch_T001 ────────────────────────────────

    #[test]
    fn spawn_init_type_mismatch_t001() {
        let (mut arena, b) = make_builtins();
        let _counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // spawn Counter "hi"   (Counter requires Int, we pass Text)
        let actor = id("Counter");
        let args = vec![text_lit("hi")];

        infer_spawn(&mut ctx, &b, &actor, &args, ds(), &arena);

        assert_eq!(ctx.errors.len(), 1, "expected T001 from type mismatch");
        assert_eq!(ctx.errors[0].code(), "T001");
        ctx.env.pop_frame();
    }

    // ── T15-13: actor_encapsulation_no_leak_ok ───────────────────────────────

    #[test]
    fn actor_encapsulation_no_leak_t019_ok() {
        // actor declares {io, fs}; both handlers union to {io, fs} — no T019.
        let io_fs = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));

        let schema = ActorSchema {
            state_fields: vec![],
            init_params: None,
            init_caps: CapabilitySet::PURE,
            handlers: vec![
                HandlerSchema {
                    name: "doIo".to_string(),
                    params: vec![],
                    ret: Type::Con(ridge_types::TyConId(0), vec![]),
                    caps: CapabilitySet::singleton(Capability::Io),
                },
                HandlerSchema {
                    name: "doFs".to_string(),
                    params: vec![],
                    ret: Type::Con(ridge_types::TyConId(0), vec![]),
                    caps: CapabilitySet::singleton(Capability::Fs),
                },
            ],
        };

        let errors = check_actor_encapsulation("MyActor", io_fs, &schema, &[], ds());
        assert!(
            errors.is_empty(),
            "expected no errors when actor caps == handler union, got {errors:?}"
        );
    }

    // ── T15-14: actor_encapsulation_handler_leak_T019 ────────────────────────

    #[test]
    fn actor_encapsulation_handler_leak_t019() {
        // actor declares {io}, one handler declares {io, fs} — T019 with {fs}.
        let actor_caps = CapabilitySet::singleton(Capability::Io);

        let schema = ActorSchema {
            state_fields: vec![],
            init_params: None,
            init_caps: CapabilitySet::PURE,
            handlers: vec![HandlerSchema {
                name: "doIoAndFs".to_string(),
                params: vec![],
                ret: Type::Con(ridge_types::TyConId(0), vec![]),
                caps: CapabilitySet::singleton(Capability::Io)
                    .union(&CapabilitySet::singleton(Capability::Fs)),
            }],
        };

        let errors = check_actor_encapsulation("MyActor", actor_caps, &schema, &[], ds());
        assert_eq!(errors.len(), 1, "expected T019, got {errors:?}");
        assert_eq!(errors[0].code(), "T019");
        if let TypeError::ActorCapabilityLeak {
            actor,
            handler,
            leaking_caps,
            ..
        } = &errors[0]
        {
            assert_eq!(actor, "MyActor");
            assert_eq!(handler, "doIoAndFs");
            assert!(
                leaking_caps.contains(Capability::Fs),
                "expected {{fs}} in leaking_caps, got {leaking_caps:?}"
            );
            assert!(
                !leaking_caps.contains(Capability::Io),
                "{{io}} must NOT be in leaking_caps"
            );
        }
    }

    // ── T15-15: caller_absorbs_only_time_for_ask ─────────────────────────────

    #[test]
    fn caller_absorbs_only_time_for_ask() {
        use crate::caps_infer::infer_caps;

        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        // body: let _ = counter ?> getCount; 1
        // caps_infer should yield only {time}.
        let ask_expr = Expr::Ask {
            handle: Box::new(Expr::Ident(id("counter"))),
            message: id("getCount"),
            args: vec![],
            timeout: None,
            span: ds(),
        };
        let body = Expr::Block(Block {
            stmts: vec![
                Expr::Let {
                    pat: ridge_ast::Pattern::Wildcard { span: ds() },
                    ty: None,
                    value: Box::new(ask_expr),
                    span: ds(),
                },
                int_lit(1),
            ],
            span: ds(),
        });

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Time),
            "Ask must propagate {{time}} to caller, got {caps:?}"
        );
        assert_eq!(
            caps.len(),
            1,
            "caller must absorb ONLY {{time}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T15-16: caller_absorbs_only_spawn_for_spawn ──────────────────────────

    #[test]
    fn caller_absorbs_only_spawn_for_spawn() {
        use crate::caps_infer::infer_caps;

        let (mut arena, b) = make_builtins();
        let _counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();

        // body: spawn Counter 0
        let body = Expr::Spawn {
            actor: id("Counter"),
            args: vec![int_lit(0)],
            span: ds(),
        };

        let caps = infer_caps(&mut ctx, &b, &body);
        assert!(
            caps.contains(Capability::Spawn),
            "Spawn must propagate {{spawn}} to caller, got {caps:?}"
        );
        assert_eq!(
            caps.len(),
            1,
            "caller must absorb ONLY {{spawn}}, got {caps:?}"
        );
        ctx.env.pop_frame();
    }

    // ── T0-TC1: ask_timeout_not_int_t026 ────────────────────────────────────────
    //
    // Phase 6 T0 (OQ-E001): `?> handler() timeout "five seconds"` must emit
    // T026 AskTimeoutNotInt because the timeout expression is Text, not Int.
    #[test]
    fn ask_timeout_not_int_t026() {
        use ridge_ast::{AskTimeout, Literal};

        let (mut arena, b) = make_builtins();
        let counter_id = register_counter(&mut arena, &b);

        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        bind_actor_handle(&mut ctx, "counter", counter_id);

        let handle = Expr::Ident(id("counter"));
        let message = id("getCount");
        // timeout expression: `"five seconds"` — type Text, not Int.
        let bad_timeout = AskTimeout::Millis(Box::new(Expr::Literal(Literal::Text {
            raw: "five seconds".to_string(),
            span: ds(),
        })));

        let ty = infer_ask(
            &mut ctx,
            &b,
            &handle,
            &message,
            &[],
            Some(&bad_timeout),
            ds(),
            &arena,
        );

        // The handler lookup succeeds (getCount exists); only the timeout
        // type-check should fail with T026.
        let t026_count = ctx.errors.iter().filter(|e| e.code() == "T026").count();
        assert_eq!(
            t026_count,
            1,
            "expected exactly 1 T026 error; got {} errors: {:?}",
            ctx.errors.len(),
            ctx.errors
        );
        // The return type should still be Int (getCount's declared return).
        // (T026 does not short-circuit handler return type inference.)
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.int),
            "Ask return type must still be Int even when timeout is maltyped; got {ty:?}"
        );
        ctx.env.pop_frame();
    }
}
