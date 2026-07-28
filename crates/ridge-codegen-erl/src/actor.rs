//! §4.28–§4.30 — Lower `IrItem::Actor` to a `gen_server` `CErlModule`.
//!
//! Each `IrActor` maps to a **separate** `CErlModule` with the `gen_server`
//! callback contract.  The caller (`module.rs::lower_module`) is responsible for
//! collecting the actor modules and returning them alongside the main module.
//!
//! ## Emitted `gen_server` callbacks (§4.28)
//!
//! | Callback | Arity | Source |
//! |---|---|---|
//! | `init/1` | 1 | `IrInit` body + state-field defaults |
//! | `handle_call/3` | 3 | one clause per handler (all handlers, per OQ-E005) |
//! | `handle_cast/2` | 2 | one clause per handler (all handlers, per OQ-E005) |
//! | `handle_info/2` | 2 | DOWN routing when `onDown` is present, else stub |
//! | `terminate/2` | 2 | `IrTerminate` body when present, else stub |
//! | `code_change/3` | 3 | boilerplate no-op stub |
//!
//! ## `start_link/N` wrapper (§4.28)
//!
//! ```erlang
//! 'start_link'/N = fun (A1, ..., AN) ->
//!     call 'gen_server':'start_link' ('?MODULE', [A1, ..., AN], [])
//! end
//! ```
//!
//! Exported always.  N = `init.params.len()` (or 0 if no init block and all
//! state fields have defaults).
//!
//! ## Both `?>` and `!` clauses (OQ-E005)
//!
//! Per resolved OQ-E005 (§8.2): every handler appears in **both**
//! `handle_call/3` and `handle_cast/2`.  `!` vs `?>` is a call-site decision;
//! constraining at codegen would break compilation when one call-site changes.
//!
//! ## Actor BEAM module name (§4.28)
//!
//! The actor module name follows the same `ridge_` prefix rule:
//! `ridge_<module_path>_<actor_name_lowercase>`.
//!
//! Since `lower_actor` receives the parent module's mangled name, the actor name
//! is appended as a suffix:
//! ```text
//! parent: "ridge_examples_url_shortener"
//! actor:  "Store"
//! result: "ridge_examples_url_shortener_store"
//! ```

// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// The boilerplate callback fns (handle_info, terminate, code_change) are single-
// purpose helpers; they never grow beyond one return.
#![allow(clippy::too_many_lines)]
// lower_actor and its private helpers are exercised through tests and via
// lower_module_all; dead_code lint fires because lower_module_all itself is
// only reachable from the actor module assembly path.
#![allow(dead_code)]

use crate::core_ast::{
    CErlAnn, CErlAtom, CErlExport, CErlExpr, CErlFn, CErlLit, CErlModule, CErlVar,
};
use crate::error::CodegenError;
use crate::expr::name_to_erl_var;
use crate::handler::{
    call_params, cast_params, lower_handler_call_clause, lower_handler_cast_clause,
};
use crate::init::{lower_handler_body_for_cast, lower_init_body, lower_terminate_body, state_var};
use crate::scope::{CodegenTables, LocalScope};
use ridge_ir::{IrActor, MailboxConfig, MailboxPolicy};
use rustc_hash::{FxHashMap, FxHasher};
use std::hash::Hasher;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Lower an [`IrActor`] to a `gen_server` [`CErlModule`].
///
/// `parent_beam_name` is the mangled BEAM module name of the parent Ridge module
/// (e.g. `"ridge_examples_url_shortener"`).  The actor's module name is derived
/// by appending `"_<actor_name_lc>"`.
///
/// `fn_arity` is the parent module's fn/const arity table so that handler bodies
/// can reference module-level fns and constants via `SymbolRef::Local`.
///
/// `tables` carries the workspace-wide codegen tables (stable beam names +
/// record metadata) so that cross-module calls and record construction inside
/// actor bodies lower correctly; empty in unit tests (legacy fallback).
///
/// # Errors
/// Returns `Err(CodegenError::IrShapeMalformed)` on Phase-5 invariant violations.
pub(crate) fn lower_actor(
    actor: &IrActor,
    parent_beam_name: &str,
    fn_arity: &FxHashMap<String, u32>,
    tables: &CodegenTables,
) -> Result<CErlModule, CodegenError> {
    // Derive actor BEAM module name: parent + "_" + actor_name_lowercase.
    let actor_name_lc = actor.name.to_lowercase();
    let actor_beam_name = format!("{parent_beam_name}_{actor_name_lc}");

    // ── Exports ───────────────────────────────────────────────────────────────
    // Required gen_server callbacks + start_link/N.
    let init_params_count = actor.init.as_ref().map_or(0, |i| i.params.len());
    let start_link_arity = u32::try_from(init_params_count).unwrap_or(0);

    let exports = vec![
        CErlExport {
            name: CErlAtom("start_link".into()),
            arity: start_link_arity,
        },
        CErlExport {
            name: CErlAtom("init".into()),
            arity: 1,
        },
        CErlExport {
            name: CErlAtom("handle_call".into()),
            arity: 3,
        },
        CErlExport {
            name: CErlAtom("handle_cast".into()),
            arity: 2,
        },
        CErlExport {
            name: CErlAtom("handle_info".into()),
            arity: 2,
        },
        CErlExport {
            name: CErlAtom("terminate".into()),
            arity: 2,
        },
        CErlExport {
            name: CErlAtom("code_change".into()),
            arity: 3,
        },
        CErlExport {
            name: CErlAtom("__ridge_mailbox_config".into()),
            arity: 0,
        },
        CErlExport {
            name: CErlAtom("__ridge_state_version".into()),
            arity: 0,
        },
    ];

    // ── Attributes ────────────────────────────────────────────────────────────
    // §6: capabilities emitted as metadata comment only (Model B erasure).
    // No runtime capability-gating attributes emitted.
    let attributes = vec![];

    // ── Functions ─────────────────────────────────────────────────────────────
    // Assemble the gen_server callbacks plus the `__ridge_mailbox_config/0`
    // accessor read by `ridge_rt:spawn_actor/3`.
    let fns_result: Result<Vec<_>, _> = [
        emit_start_link(&actor_beam_name, init_params_count),
        emit_init(actor, tables),
        emit_handle_call(actor, fn_arity, parent_beam_name, tables),
        emit_handle_cast(actor, fn_arity, parent_beam_name, tables),
        emit_handle_info(actor, fn_arity, parent_beam_name, tables),
        emit_terminate(actor, fn_arity, parent_beam_name, tables),
        Ok(emit_code_change_stub()),
        Ok(emit_mailbox_config_fn(actor)),
        Ok(emit_state_version_fn(actor)),
    ]
    .into_iter()
    .collect();
    let mut fns = fns_result?;

    // §4.28: doc comment as annotation.
    // We add it as an attribute annotation rather than a function annotation
    // since it applies to the whole module.
    if let Some(doc) = &actor.doc {
        let first_line = doc.lines().next().unwrap_or("").trim();
        if !first_line.is_empty() {
            // Emit a doc annotation on the start_link fn (the module's public entry).
            if let Some(sl) = fns.first_mut() {
                sl.anns.push(CErlAnn(format!("%% Doc: {first_line}")));
            }
        }
    }

    Ok(CErlModule {
        name: CErlAtom(actor_beam_name),
        exports,
        attributes,
        fns,
    })
}

// ── start_link/N ─────────────────────────────────────────────────────────────

/// Emit the `start_link/N` wrapper function (§4.28).
///
/// ```erlang
/// 'start_link'/N = fun (A1, ..., AN) ->
///     call 'gen_server':'start_link' ('?MODULE', [A1, ..., AN], [])
/// end
/// ```
fn emit_start_link(actor_beam_name: &str, n_params: usize) -> Result<CErlFn, CodegenError> {
    let arity = u32::try_from(n_params).map_err(|_| CodegenError::IrShapeMalformed {
        variant: "IrActor",
        span: ridge_ast::Span::point(0),
        detail: format!("actor start_link arity {n_params} exceeds u32 — cannot emit"),
    })?;

    // Build parameter variable list: [V_A1, V_A2, ..., V_AN].
    let params: Vec<CErlVar> = (0..n_params).map(|i| CErlVar(format!("V_A{i}"))).collect();

    // Build the args list literal: [V_A1, ..., V_AN].
    let args_list = CErlExpr::ListLit(params.iter().map(|v| CErlExpr::Var(v.clone())).collect());

    // call 'gen_server':'start_link' ('?MODULE', [A1, ..., AN], [])
    // NOTE: '?MODULE' is a macro in source Erlang; in Core Erlang we emit the
    // actual atom of the callback module.
    let call_expr = CErlExpr::Call {
        module: CErlAtom("gen_server".into()),
        fn_name: CErlAtom("start_link".into()),
        args: vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom(actor_beam_name.into()))),
            args_list,
            CErlExpr::Lit(CErlLit::Nil), // Options: []
        ],
    };

    let body = CErlExpr::Fun {
        params,
        body: Box::new(call_expr),
    };

    Ok(CErlFn {
        name: CErlAtom("start_link".into()),
        arity,
        anns: vec![CErlAnn(
            "%% Ridge gen_server start_link wrapper (§4.28)".into(),
        )],
        body,
    })
}

// ── init/1 ────────────────────────────────────────────────────────────────────

/// Emit the `gen_server:init/1` callback (§4.29).
///
/// ```erlang
/// 'init'/1 = fun (V_Args) ->
///     <lower_init_body(actor.init, actor.state_fields)>
/// end
/// ```
///
/// The `Args` parameter is bound as `V_Args`; the init body may destructure it
/// via pattern matching in the user's init params.
///
/// # Init failure (OQ-E006)
/// If the init body raises, `gen_server:start_link/3` propagates the failure.
/// `ridge_rt:spawn_actor/3` re-raises it (BEAM-crash).  This is the deliberate
/// language-level asymmetry (§4.29).
fn emit_init(actor: &IrActor, tables: &CodegenTables) -> Result<CErlFn, CodegenError> {
    // §4.29: the init body lowering handles both the default-map case and the
    // user-body case.
    let init_body_expr =
        lower_init_body(actor.init.as_ref(), &actor.state_fields, actor.span, tables)?;

    // Actors with a `terminate` callback trap exits so the supervisor's
    // `shutdown` exit signal is converted into a `terminate/2` call instead
    // of instant death — a gen_server that does not trap exits never runs
    // terminate on an ordered stop. This is internal and not observable:
    // unsupervised actors hold no links (spawn is unlinked), so no other
    // EXIT messages can arrive.
    let init_body_expr = if actor.terminate.is_some() {
        CErlExpr::Do {
            first: Box::new(CErlExpr::Call {
                module: CErlAtom("erlang".into()),
                fn_name: CErlAtom("process_flag".into()),
                args: vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("trap_exit".into()))),
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                ],
            }),
            then: Box::new(init_body_expr),
        }
    } else {
        init_body_expr
    };

    // The init/1 parameter is the Args list (passed from start_link).
    // For actors with no init block, Args is ignored; we bind it as V_Args anyway
    // so the gen_server contract is satisfied.
    let args_var = CErlVar("V_Args".into());

    let body = CErlExpr::Fun {
        params: vec![args_var],
        body: Box::new(init_body_expr),
    };

    Ok(CErlFn {
        name: CErlAtom("init".into()),
        arity: 1,
        anns: vec![CErlAnn(
            "%% gen_server:init/1 — state initialisation (§4.29)".into(),
        )],
        body,
    })
}

// ── handle_call/3 ─────────────────────────────────────────────────────────────

/// Emit the `gen_server:handle_call/3` callback (§4.30, OQ-E005).
///
/// One clause per handler in `actor.dispatch`, in source order.
/// Falls through to a default `{stop, unexpected_call, ok}` clause.
///
/// Per OQ-E005 (§8.2): all handlers emit into `handle_call/3` regardless of
/// return type — `!` vs `?>` is a call-site decision.
// OQ-E005: emit all handlers in handle_call (plan §4.30 + §8.2).
fn emit_handle_call(
    actor: &IrActor,
    fn_arity: &FxHashMap<String, u32>,
    parent_beam_name: &str,
    tables: &CodegenTables,
) -> Result<CErlFn, CodegenError> {
    let mut clauses = Vec::with_capacity(actor.dispatch.len() + 1);

    for handler in &actor.dispatch {
        // B-6: pass parent module id + beam name so handler bodies can emit
        // qualified calls for cross-module SymbolRef::Local references.
        clauses.push(lower_handler_call_clause(
            handler,
            fn_arity,
            actor.module,
            parent_beam_name,
            tables,
        )?);
    }

    // Defensive catch-all clause: {stop, unexpected_call, ok}.
    clauses.push(unexpected_call_clause());

    // Fun params: (V_Msg, _V_From, V_StateArg)
    let params = call_params();

    let body = CErlExpr::Fun {
        params,
        body: Box::new(CErlExpr::Case {
            scrutinee: Box::new(CErlExpr::Var(CErlVar("V_Msg".into()))),
            clauses,
        }),
    };

    Ok(CErlFn {
        name: CErlAtom("handle_call".into()),
        arity: 3,
        anns: vec![CErlAnn(
            "%% gen_server:handle_call/3 — ask handlers (§4.30, OQ-E005)".into(),
        )],
        body,
    })
}

// ── handle_cast/2 ─────────────────────────────────────────────────────────────

/// Emit the `gen_server:handle_cast/2` callback (§4.30, OQ-E005).
///
/// One clause per handler in `actor.dispatch`, in source order.
/// Falls through to a default `{noreply, State}` clause.
///
/// Per OQ-E005 (§8.2): all handlers emit into `handle_cast/2` regardless of
/// return type.
// OQ-E005: emit all handlers in handle_cast (plan §4.30 + §8.2).
fn emit_handle_cast(
    actor: &IrActor,
    fn_arity: &FxHashMap<String, u32>,
    parent_beam_name: &str,
    tables: &CodegenTables,
) -> Result<CErlFn, CodegenError> {
    let mut clauses = Vec::with_capacity(actor.dispatch.len() + 1);

    for handler in &actor.dispatch {
        // B-6: pass parent module id + beam name so handler bodies can emit
        // qualified calls for cross-module SymbolRef::Local references.
        clauses.push(lower_handler_cast_clause(
            handler,
            fn_arity,
            actor.module,
            parent_beam_name,
            tables,
        )?);
    }

    // Defensive catch-all clause: {noreply, V_StateArg} (ignore unknown casts).
    clauses.push(unexpected_cast_clause());

    // Fun params: (V_Msg, V_StateArg)
    let params = cast_params();

    let body = CErlExpr::Fun {
        params,
        body: Box::new(CErlExpr::Case {
            scrutinee: Box::new(CErlExpr::Var(CErlVar("V_Msg".into()))),
            clauses,
        }),
    };

    Ok(CErlFn {
        name: CErlAtom("handle_cast".into()),
        arity: 2,
        anns: vec![CErlAnn(
            "%% gen_server:handle_cast/2 — send handlers (§4.30, OQ-E005)".into(),
        )],
        body,
    })
}

// ── Boilerplate stubs ─────────────────────────────────────────────────────────

/// `handle_info/2` no-op stub.
///
/// Ignores unknown messages; returns `{noreply, State}`.
fn emit_handle_info_stub() -> CErlFn {
    // fun (_Msg, State) -> {noreply, State} end
    let params = vec![CErlVar("_V_InfoMsg".into()), CErlVar("V_InfoState".into())];
    let body = CErlExpr::Fun {
        params,
        body: Box::new(CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("noreply".into()))),
            CErlExpr::Var(CErlVar("V_InfoState".into())),
        ])),
    };
    CErlFn {
        name: CErlAtom("handle_info".into()),
        arity: 2,
        anns: vec![CErlAnn(
            "%% gen_server:handle_info/2 — boilerplate no-op stub (§4.28)".into(),
        )],
        body,
    }
}

/// Emit the `gen_server:handle_info/2` callback.
///
/// Actors without an `onDown` member keep the no-op stub (byte-identical
/// output). When the member is present, DOWN messages from `Actor.monitor`
/// are routed into the user body; every other info message is still
/// discarded:
///
/// ```erlang
/// 'handle_info'/2 = fun (V_InfoMsg, V_InfoState) ->
///     case V_InfoMsg of
///         {'DOWN', V_DownRef, 'process', _V_DownPid, V_DownReason} ->
///             let <m-param> = V_DownRef in
///             let <reason-param> = call 'ridge_rt':'exit_reason_to_ridge'(V_DownReason) in
///             let V_State = V_InfoState in
///                 <user body; every leaf yields {'noreply', V_State_n}>
///         _ -> {'noreply', V_InfoState}
///     end
/// end
/// ```
///
/// The OTP reason is mapped to the `ExitReason` union at the runtime
/// boundary so the body matches on `'Normal'` / `'NotRunning'` /
/// `'Shutdown'` / `{'Crashed', Text}`. At most two user parameters are
/// supported (the monitor reference and the reason); more would have no
/// OTP counterpart.
fn emit_handle_info(
    actor: &IrActor,
    fn_arity: &FxHashMap<String, u32>,
    parent_beam_name: &str,
    tables: &CodegenTables,
) -> Result<CErlFn, CodegenError> {
    let Some(d) = actor.on_down.as_ref() else {
        return Ok(emit_handle_info_stub());
    };

    if d.params.len() > 2 {
        return Err(CodegenError::IrShapeMalformed {
            variant: "IrOnDown",
            span: d.span,
            detail: format!(
                "onDown declares {} params — OTP delivers exactly two (the monitor reference and the exit reason)",
                d.params.len()
            ),
        });
    }

    let mut scope = LocalScope::with_actor_parent(fn_arity.clone(), actor.module, parent_beam_name);
    scope.tables = tables.clone();
    let mut state_idx: u32 = 0;

    let lowered_body = lower_handler_body_for_cast(&d.body, &mut scope, &mut state_idx, d.span)?;

    // let V_State = V_InfoState in <body ending in {'noreply', V_State_n}>
    let state_bind = CErlExpr::Let {
        var: state_var(0),
        value: Box::new(CErlExpr::Var(CErlVar("V_InfoState".into()))),
        body: Box::new(lowered_body),
    };

    // Bind the declared params (innermost-out): the monitor reference to the
    // raw DOWN ref, the reason to the mapped `ExitReason` wire value. A
    // missing param simply ignores the value.
    let mut body_expr = state_bind;
    if let Some(reason_param) = d.params.get(1) {
        body_expr = CErlExpr::Let {
            var: CErlVar(name_to_erl_var(&reason_param.name)),
            value: Box::new(CErlExpr::Call {
                module: CErlAtom("ridge_rt".into()),
                fn_name: CErlAtom("exit_reason_to_ridge".into()),
                args: vec![CErlExpr::Var(CErlVar("V_DownReason".into()))],
            }),
            body: Box::new(body_expr),
        };
    }
    if let Some(monitor_param) = d.params.first() {
        body_expr = CErlExpr::Let {
            var: CErlVar(name_to_erl_var(&monitor_param.name)),
            value: Box::new(CErlExpr::Var(CErlVar("V_DownRef".into()))),
            body: Box::new(body_expr),
        };
    }

    let down_clause = crate::core_ast::CErlClause {
        pattern: crate::core_ast::CErlPat::Tuple(vec![
            crate::core_ast::CErlPat::Lit(CErlLit::Atom(CErlAtom("DOWN".into()))),
            crate::core_ast::CErlPat::Var(CErlVar("V_DownRef".into())),
            crate::core_ast::CErlPat::Lit(CErlLit::Atom(CErlAtom("process".into()))),
            crate::core_ast::CErlPat::Wild,
            crate::core_ast::CErlPat::Var(CErlVar("V_DownReason".into())),
        ]),
        guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
        body: body_expr,
    };

    let other_clause = crate::core_ast::CErlClause {
        pattern: crate::core_ast::CErlPat::Wild,
        guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
        body: CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("noreply".into()))),
            CErlExpr::Var(CErlVar("V_InfoState".into())),
        ]),
    };

    let body = CErlExpr::Fun {
        params: vec![CErlVar("V_InfoMsg".into()), CErlVar("V_InfoState".into())],
        body: Box::new(CErlExpr::Case {
            scrutinee: Box::new(CErlExpr::Var(CErlVar("V_InfoMsg".into()))),
            clauses: vec![down_clause, other_clause],
        }),
    };

    Ok(CErlFn {
        name: CErlAtom("handle_info".into()),
        arity: 2,
        anns: vec![CErlAnn(
            "%% gen_server:handle_info/2 — monitor DOWN routing (§4.28)".into(),
        )],
        body,
    })
}

/// `terminate/2` no-op stub.
///
/// Returns `'ok'` (Unit).
fn emit_terminate_stub() -> CErlFn {
    // fun (_Reason, _State) -> 'ok' end
    let params = vec![
        CErlVar("_V_TermReason".into()),
        CErlVar("_V_TermState".into()),
    ];
    let body = CErlExpr::Fun {
        params,
        body: Box::new(CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into())))),
    };
    CErlFn {
        name: CErlAtom("terminate".into()),
        arity: 2,
        anns: vec![CErlAnn(
            "%% gen_server:terminate/2 — boilerplate no-op stub (§4.28)".into(),
        )],
        body,
    }
}

/// Emit the `gen_server:terminate/2` callback.
///
/// Actors without a `terminate` member keep the no-op stub. When the member
/// is present, emits:
///
/// ```erlang
/// 'terminate'/2 = fun (V_TermReason, V_StateArg) ->
///     let V_Reason = call 'ridge_rt':'exit_reason_to_ridge'(V_TermReason) in
///     let V_State = V_StateArg in
///         <user body; every leaf sequences its value and yields 'ok'>
/// ```
///
/// The OTP reason is mapped to the `ExitReason` union at the runtime boundary
/// so the body matches on `'Normal'` / `'Shutdown'` / `{'Crashed', Text}`.
/// At most one user parameter is supported (the reason); more would have no
/// OTP counterpart.
fn emit_terminate(
    actor: &IrActor,
    fn_arity: &FxHashMap<String, u32>,
    parent_beam_name: &str,
    tables: &CodegenTables,
) -> Result<CErlFn, CodegenError> {
    let Some(t) = actor.terminate.as_ref() else {
        return Ok(emit_terminate_stub());
    };

    if t.params.len() > 1 {
        return Err(CodegenError::IrShapeMalformed {
            variant: "IrTerminate",
            span: t.span,
            detail: format!(
                "terminate declares {} params — OTP delivers exactly one (the exit reason)",
                t.params.len()
            ),
        });
    }

    let mut scope = LocalScope::with_actor_parent(fn_arity.clone(), actor.module, parent_beam_name);
    scope.tables = tables.clone();
    let mut state_idx: u32 = 0;

    let lowered_body = lower_terminate_body(&t.body, &mut scope, &mut state_idx, t.span)?;

    // let V_State = V_StateArg in <body ending in 'ok' at each leaf>
    let state_bind = CErlExpr::Let {
        var: state_var(0),
        value: Box::new(CErlExpr::Var(CErlVar("V_StateArg".into()))),
        body: Box::new(lowered_body),
    };

    // let <reason-param> = call 'ridge_rt':'exit_reason_to_ridge'(V_TermReason)
    // — bound only when the user declared the param; `terminate io () = …`
    // ignores the reason entirely.
    let body_expr = if let Some(reason_param) = t.params.first() {
        CErlExpr::Let {
            var: CErlVar(name_to_erl_var(&reason_param.name)),
            value: Box::new(CErlExpr::Call {
                module: CErlAtom("ridge_rt".into()),
                fn_name: CErlAtom("exit_reason_to_ridge".into()),
                args: vec![CErlExpr::Var(CErlVar("V_TermReason".into()))],
            }),
            body: Box::new(state_bind),
        }
    } else {
        state_bind
    };

    let body = CErlExpr::Fun {
        params: vec![CErlVar("V_TermReason".into()), CErlVar("V_StateArg".into())],
        body: Box::new(body_expr),
    };

    Ok(CErlFn {
        name: CErlAtom("terminate".into()),
        arity: 2,
        anns: vec![CErlAnn(
            "%% gen_server:terminate/2 — user terminate callback (§4.28)".into(),
        )],
        body,
    })
}

/// `code_change/3` no-op stub.
///
/// Returns `{ok, State}`.
fn emit_code_change_stub() -> CErlFn {
    // fun (_OldVsn, State, _Extra) -> {ok, State} end
    let params = vec![
        CErlVar("_V_OldVsn".into()),
        CErlVar("V_CcState".into()),
        CErlVar("_V_Extra".into()),
    ];
    let body = CErlExpr::Fun {
        params,
        body: Box::new(CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
            CErlExpr::Var(CErlVar("V_CcState".into())),
        ])),
    };
    CErlFn {
        name: CErlAtom("code_change".into()),
        arity: 3,
        anns: vec![CErlAnn(
            "%% gen_server:code_change/3 — pass-through (state shape unchanged); the code loader adds migration dispatch".into(),
        )],
        body,
    }
}

// ── __ridge_state_version/0 ───────────────────────────────────────────────────

/// Emit the `'__ridge_state_version'/0` accessor: a constant term
/// `{ActorName, StateLayoutHash}` identifying the actor's state-field layout.
/// The runtime code loader reads it to decide whether a suspended actor's
/// state needs migration before the new code resumes it.
fn emit_state_version_fn(actor: &IrActor) -> CErlFn {
    let mut h = FxHasher::default();
    h.write(actor.name.as_bytes());
    for field in &actor.state_fields {
        h.write(field.name.as_bytes());
        h.write(b":");
        h.write(format!("{:?}", field.ty).as_bytes());
        h.write(b";");
    }
    #[allow(clippy::cast_possible_wrap)]
    let term = CErlExpr::Tuple(vec![
        CErlExpr::Lit(CErlLit::Atom(CErlAtom(actor.name.clone()))),
        CErlExpr::Lit(CErlLit::Int(h.finish() as i64)),
    ]);
    CErlFn {
        name: CErlAtom("__ridge_state_version".into()),
        arity: 0,
        anns: vec![CErlAnn(
            "%% Ridge actor state layout version (read by the code loader)".into(),
        )],
        body: CErlExpr::Fun {
            params: vec![],
            body: Box::new(term),
        },
    }
}

// ── __ridge_mailbox_config/0 ──────────────────────────────────────────────────

/// Emit the `'__ridge_mailbox_config'/0` accessor read by
/// `ridge_rt:spawn_actor/3`.
///
/// The accessor returns the mailbox configuration as an Erlang term that
/// matches the runtime's pattern table:
///
/// | Ridge declaration                  | Returned term                    |
/// |------------------------------------|----------------------------------|
/// | (omitted) or `mailbox unbounded`   | `unbounded`                      |
/// | `mailbox bounded N drop newest`    | `{bounded, N, drop_newest}`      |
/// | `mailbox bounded N error`          | `{bounded, N, error}`            |
///
/// The function is module-level and takes no arguments, so `spawn_actor`
/// can look it up once at spawn time and bake the result into the handle.
fn emit_mailbox_config_fn(actor: &IrActor) -> CErlFn {
    let config_term = mailbox_config_term(actor.mailbox_config.as_ref());
    let body = CErlExpr::Fun {
        params: vec![],
        body: Box::new(config_term),
    };
    CErlFn {
        name: CErlAtom("__ridge_mailbox_config".into()),
        arity: 0,
        anns: vec![CErlAnn(
            "%% Ridge mailbox config accessor (read by ridge_rt:spawn_actor/3)".into(),
        )],
        body,
    }
}

/// Translate the optional `IrActor.mailbox_config` into the Erlang term
/// returned by `__ridge_mailbox_config/0`.
fn mailbox_config_term(config: Option<&MailboxConfig>) -> CErlExpr {
    let atom = |s: &str| CErlExpr::Lit(CErlLit::Atom(CErlAtom(s.into())));
    match config {
        None | Some(MailboxConfig::Unbounded) => atom("unbounded"),
        Some(MailboxConfig::Bounded { capacity, policy }) => CErlExpr::Tuple(vec![
            atom("bounded"),
            CErlExpr::Lit(CErlLit::Int(*capacity)),
            atom(match policy {
                MailboxPolicy::DropNewest => "drop_newest",
                MailboxPolicy::DropOldest => "drop_oldest",
                MailboxPolicy::Error => "error",
            }),
        ]),
    }
}

// ── Defensive catch-all clauses ───────────────────────────────────────────────

/// Catch-all clause for `handle_call/3`: stops the server on unknown call.
///
/// ```erlang
/// _ when 'true' -> {stop, unexpected_call, ok}
/// ```
fn unexpected_call_clause() -> crate::core_ast::CErlClause {
    crate::core_ast::CErlClause {
        pattern: crate::core_ast::CErlPat::Wild,
        guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
        body: CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("stop".into()))),
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("unexpected_call".into()))),
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
        ]),
    }
}

/// Catch-all clause for `handle_cast/2`: ignores unknown casts.
///
/// ```erlang
/// _ when 'true' -> {noreply, V_StateArg}
/// ```
fn unexpected_cast_clause() -> crate::core_ast::CErlClause {
    crate::core_ast::CErlClause {
        pattern: crate::core_ast::CErlPat::Wild,
        guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
        body: CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("noreply".into()))),
            CErlExpr::Var(CErlVar("V_StateArg".into())),
        ]),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit};
    use ridge_ast::Span;
    use ridge_ir::{IrActor, IrExpr, IrHandler, IrLit, IrNodeId, IrParam, IrStateField};
    use ridge_resolve::{ModuleId, NodeId};
    use ridge_types::{CapabilitySet, TyConId, Type};

    fn sp() -> Span {
        Span::point(0)
    }

    fn lit_unit() -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Unit,
            span: sp(),
        }
    }

    fn lit_int(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: IrNodeId(0),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    fn make_state_field(name: &str, default: Option<IrExpr>) -> IrStateField {
        IrStateField {
            name: name.into(),
            ty: Type::Error, // PHASE7-STUB
            default,
            span: sp(),
        }
    }

    fn make_handler(name: &str) -> IrHandler {
        IrHandler {
            message_name: name.into(),
            params: vec![],
            ret_ty: Type::Error, // PHASE7-STUB
            caps: CapabilitySet::PURE,
            body: lit_unit(),
            origin: NodeId(0),
            span: sp(),
            doc: None,
        }
    }

    fn make_actor(
        name: &str,
        handlers: Vec<IrHandler>,
        state_fields: Vec<IrStateField>,
    ) -> IrActor {
        IrActor {
            name: name.into(),
            module: ModuleId(0),
            tycon: TyConId(0),
            state_fields,
            init: None,
            terminate: None,
            on_down: None,
            dispatch: handlers,
            mailbox_config: None,
            origin: NodeId(0),
            span: sp(),
            is_pub: true,
            doc: None,
        }
    }

    // §4.28 — Actor module emission

    #[test]
    fn actor_module_name_derives_from_parent_and_actor() {
        let actor = make_actor("Limiter", vec![], vec![]);
        let m = lower_actor(
            &actor,
            "ridge_examples_rate_limiter",
            &FxHashMap::default(),
            &CodegenTables::default(),
        )
        .unwrap();
        assert_eq!(m.name.0, "ridge_examples_rate_limiter_limiter");
    }

    #[test]
    fn actor_module_exports_gen_server_callbacks() {
        let actor = make_actor("Store", vec![], vec![]);
        let m = lower_actor(
            &actor,
            "ridge_examples_url_shortener",
            &FxHashMap::default(),
            &CodegenTables::default(),
        )
        .unwrap();

        let exported_names: Vec<&str> = m.exports.iter().map(|e| e.name.0.as_str()).collect();
        assert!(exported_names.contains(&"init"), "must export init/1");
        assert!(
            exported_names.contains(&"handle_call"),
            "must export handle_call/3"
        );
        assert!(
            exported_names.contains(&"handle_cast"),
            "must export handle_cast/2"
        );
        assert!(
            exported_names.contains(&"handle_info"),
            "must export handle_info/2"
        );
        assert!(
            exported_names.contains(&"terminate"),
            "must export terminate/2"
        );
        assert!(
            exported_names.contains(&"code_change"),
            "must export code_change/3"
        );
        assert!(
            exported_names.contains(&"start_link"),
            "must export start_link"
        );
        assert!(
            exported_names.contains(&"__ridge_mailbox_config"),
            "must export __ridge_mailbox_config/0 (read by ridge_rt:spawn_actor)"
        );
    }

    #[test]
    fn actor_module_has_nine_fns() {
        // start_link + init + handle_call + handle_cast + handle_info +
        // terminate + code_change + __ridge_mailbox_config +
        // __ridge_state_version.
        let actor = make_actor("Counter", vec![], vec![]);
        let m = lower_actor(
            &actor,
            "ridge_examples",
            &FxHashMap::default(),
            &CodegenTables::default(),
        )
        .unwrap();
        assert_eq!(
            m.fns.len(),
            9,
            "actor module must have exactly 9 callback fns"
        );
        assert!(
            m.fns.iter().any(|f| f.name.0 == "__ridge_state_version"),
            "actor module must expose __ridge_state_version/0"
        );
        assert!(
            m.exports
                .iter()
                .any(|e| e.name.0 == "__ridge_state_version"),
            "__ridge_state_version/0 must be exported"
        );
    }

    // §4.28 — start_link/N

    #[test]
    fn start_link_arity_matches_init_params() {
        let mut actor = make_actor("Limiter", vec![], vec![]);
        // Add an init block with 2 params.
        actor.init = Some(ridge_ir::IrInit {
            params: vec![
                IrParam {
                    name: "max_rate".into(),
                    ty: Type::Error,
                    span: sp(),
                },
                IrParam {
                    name: "window_ms".into(),
                    ty: Type::Error,
                    span: sp(),
                },
            ],
            caps: CapabilitySet::PURE,
            body: lit_unit(),
            span: sp(),
        });
        let m = lower_actor(
            &actor,
            "ridge_examples_rate_limiter",
            &FxHashMap::default(),
            &CodegenTables::default(),
        )
        .unwrap();

        let sl = m.fns.iter().find(|f| f.name.0 == "start_link").unwrap();
        assert_eq!(sl.arity, 2, "start_link arity must match init param count");
    }

    // OQ-E005 — Both call and cast clauses present

    #[test]
    #[allow(clippy::items_after_statements)]
    fn handle_call_and_cast_both_have_handler_clauses() {
        let handlers = vec![make_handler("increment"), make_handler("get_count")];
        let actor = make_actor("Counter", handlers, vec![]);
        let m = lower_actor(
            &actor,
            "ridge_examples",
            &FxHashMap::default(),
            &CodegenTables::default(),
        )
        .unwrap();

        let handle_call = m.fns.iter().find(|f| f.name.0 == "handle_call").unwrap();
        let handle_cast = m.fns.iter().find(|f| f.name.0 == "handle_cast").unwrap();

        // Both must have a Case with 3 clauses: 2 handlers + 1 catch-all.
        fn count_clauses(fn_: &CErlFn) -> usize {
            match &fn_.body {
                CErlExpr::Fun { body, .. } => match body.as_ref() {
                    CErlExpr::Case { clauses, .. } => clauses.len(),
                    _ => 0,
                },
                _ => 0,
            }
        }

        assert_eq!(
            count_clauses(handle_call),
            3,
            "handle_call: 2 handlers + 1 catch-all"
        );
        assert_eq!(
            count_clauses(handle_cast),
            3,
            "handle_cast: 2 handlers + 1 catch-all"
        );
    }

    // §4.28 — Boilerplate stubs

    #[test]
    fn terminate_stub_returns_ok() {
        let stub = emit_terminate_stub();
        assert_eq!(stub.name.0, "terminate");
        assert_eq!(stub.arity, 2);
        match &stub.body {
            CErlExpr::Fun { body, .. } => {
                assert!(
                    matches!(body.as_ref(), CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "ok"),
                    "terminate must return 'ok'"
                );
            }
            other => panic!("expected Fun, got {other:?}"),
        }
    }

    #[test]
    fn code_change_stub_returns_ok_state_tuple() {
        let stub = emit_code_change_stub();
        assert_eq!(stub.name.0, "code_change");
        assert_eq!(stub.arity, 3);
        match &stub.body {
            CErlExpr::Fun { body, .. } => match body.as_ref() {
                CErlExpr::Tuple(elems) => {
                    assert!(
                        matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "ok")
                    );
                }
                other => panic!("expected Tuple, got {other:?}"),
            },
            other => panic!("expected Fun, got {other:?}"),
        }
    }

    // §4.29 — init/1 with state fields

    #[test]
    fn init_with_default_state_fields_emits_ok_map() {
        let fields = vec![make_state_field("count", Some(lit_int(0)))];
        let actor = make_actor("Counter", vec![], fields);
        let m = lower_actor(
            &actor,
            "ridge_examples",
            &FxHashMap::default(),
            &CodegenTables::default(),
        )
        .unwrap();

        let init_fn = m.fns.iter().find(|f| f.name.0 == "init").unwrap();
        assert_eq!(init_fn.arity, 1);
    }
}
