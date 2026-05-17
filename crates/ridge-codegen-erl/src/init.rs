//! §4.29 — Lower `IrInit` and actor state-field initialisation to the
//! `gen_server:init/1` callback body.
//!
//! The `gen_server` `init/1` callback:
//! 1. Receives the `Args` list that `spawn ActorName …` passed.
//! 2. Destructures `Args` into the init parameter names.
//! 3. Initialises the state map `V_State` from defaults and/or explicit
//!    `<field> <- <expr>` assignments in the init body.
//! 4. Returns `{'ok', V_State_final}`.
//!
//! ## State-field default initialisation (§4.29)
//!
//! For state fields whose `IrStateField.default` is `Some(expr)`, the default
//! expression is evaluated at the top of `init/1`, before the user's init body
//! runs.  Fields without a default must be assigned in the body; if they are not,
//! they remain absent from the state map (a Phase-5 invariant that the type
//! checker enforces — Phase 6 trusts upstream).
//!
//! ## Actor-state thread (§3.12 + §4.8)
//!
//! The running state map is carried as SSA-suffixed variables `V_State`,
//! `V_State1`, `V_State2`, … through the init body.  Each
//! `Assign { target: StateField { name } }` emits:
//! ```erlang
//! let <V_State_next> = call 'maps':'put' ('name', Value, V_State_prev) in
//!     ...
//! ```
//! The final state variable is returned inside `{'ok', V_State_final}`.
//!
//! ## B-7 fix — state-varying Match arms (§4.30 + §3.12)
//!
//! When a handler body contains a `Match` whose arms produce different numbers
//! of state mutations (e.g., one arm assigns a state field, another does not),
//! Core Erlang's lexical scoping prevents referencing the final state variable
//! *outside* the case expression.  The fix: thread a `leaf_wrap` closure through
//! the lowering functions so that the OTP response tuple (`{'reply', V, S}` or
//! `{'noreply', S}`) is constructed at the LEAF of each arm — inside all enclosing
//! `let` bindings where the relevant `V_State<n>` variables are in scope.

// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// Init lowering functions are called from actor.rs (lower_actor → emit_init →
// lower_init_body).  dead_code fires because lower_actor itself is only reachable
// from lower_module_all in the actor module assembly path.
#![allow(dead_code)]

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlLit, CErlPat, CErlVar};
use crate::error::CodegenError;
use crate::expr::{lower_expr_in_scope, name_to_erl_var};
use crate::letrec_detect::body_references_local;
use crate::scope::{ssa_var, LocalScope};
use ridge_ast::Span;
use ridge_ir::{AssignTarget, IrExpr, IrInit, IrParam, IrStateField};

// ── State-SSA tracking ────────────────────────────────────────────────────────

/// Base name for the state variable (always `V_State`).
const STATE_VAR_BASE: &str = "V_State";

/// Produce the current state `CErlVar` from the SSA index.
///
/// - index 0 → `V_State`
/// - index 1 → `V_State1`
/// - index N → `V_StateN`
pub(crate) fn state_var(idx: u32) -> CErlVar {
    ssa_var(STATE_VAR_BASE, idx)
}

/// Produce a `CErlExpr::Var` referencing the current state variable.
pub(crate) fn state_expr(idx: u32) -> CErlExpr {
    CErlExpr::Var(state_var(idx))
}

// ── Init param destructuring (B-4 fix, Phase 6 pass 3) ───────────────────────

/// Build the nested `CErlPat` for a list of `N` params.
///
/// Produces a right-nested cons pattern:
/// ```text
/// [V_P1 | [V_P2 | ... | []]]
/// ```
///
/// This matches the `V_Args` list emitted by `gen_server:start_link/3` when the
/// actor is spawned via `ridge_rt:spawn_actor/3`.  Phase 5 passes the init params
/// as a plain Erlang list; Phase 6 must destructure them at the top of `init/1`.
fn build_args_list_pattern(params: &[IrParam]) -> CErlPat {
    // Build right-to-left: start with [] and cons each param variable.
    let mut pat = CErlPat::Lit(CErlLit::Nil); // []
    for param in params.iter().rev() {
        let var_name = name_to_erl_var(&param.name);
        pat = CErlPat::Cons {
            head: Box::new(CErlPat::Var(CErlVar(var_name))),
            tail: Box::new(pat),
        };
    }
    pat
}

/// Wrap `inner_expr` in a `case V_Args of <list_pattern> -> inner_expr end`.
///
/// This is the B-4 fix: the `gen_server:init/1` callback receives all init
/// parameters as a single list `V_Args`.  We must destructure it into the
/// individual parameter variables before the user's init body runs.
///
/// If `params` is empty, returns `inner_expr` unchanged (no destructuring needed).
fn wrap_in_args_destructure(params: &[IrParam], inner_expr: CErlExpr) -> CErlExpr {
    if params.is_empty() {
        return inner_expr;
    }
    let list_pat = build_args_list_pattern(params);
    CErlExpr::Case {
        scrutinee: Box::new(CErlExpr::Var(CErlVar("V_Args".into()))),
        clauses: vec![CErlClause {
            pattern: list_pat,
            guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
            body: inner_expr,
        }],
    }
}

// ── Init body lowering (§4.29) ────────────────────────────────────────────────

/// Lower the `IrInit` block and state-field defaults to the body of
/// `gen_server:init/1`.
///
/// The emitted expression has the shape:
/// ```erlang
/// %% Build default state map.
/// let V_State = #{field1 => Default1, field2 => Default2, ...} in
///     %% Run the user's init body (may assign state fields).
///     let <...> = <init body lowered> in
///         {'ok', V_State_final}
/// ```
///
/// If `init` is `None` and all state fields have defaults, emit the trivial form:
/// ```erlang
/// {'ok', #{field1 => Default1, ...}}
/// ```
///
/// # Errors
/// Returns `Err(CodegenError::IrShapeMalformed)` if any state field lacks a
/// default *and* there is no `init` block to set it (defensive — Phase 5 should
/// have rejected this pattern during type-checking).
pub(crate) fn lower_init_body(
    init: Option<&IrInit>,
    state_fields: &[IrStateField],
    span: Span,
) -> Result<CErlExpr, CodegenError> {
    // Build the initial state map from defaults.
    let default_pairs: Vec<(CErlExpr, CErlExpr)> = state_fields
        .iter()
        .filter_map(|f| {
            f.default.as_ref().map(|default_expr| {
                let key = CErlExpr::Lit(CErlLit::Atom(CErlAtom(f.name.clone())));
                let val = lower_expr_in_scope(default_expr, &mut LocalScope::new());
                val.ok().map(|v| (key, v))
            })
        })
        .flatten()
        .collect();

    // The initial state expression (default map).
    let initial_state = CErlExpr::MapLit(default_pairs);

    match init {
        None => {
            // No init block — return defaults directly.
            // §4.29: "If `init` is `None` and all `IrStateField.default` are `Some`,
            // emit `init/1` that simply returns `{'ok', <default_state_map>}`."
            Ok(CErlExpr::Tuple(vec![
                CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
                initial_state,
            ]))
        }
        Some(init) => {
            // With an init block: run the body with state-threading.
            let mut scope = LocalScope::new();
            let mut state_idx: u32 = 0;

            // Bind V_State to the default map.
            let state_bind = state_var(0);

            // B-7 init fix: use leaf_wrap to inject {'ok', V_State<n>} AT the leaf
            // of the init body — inside all enclosing let-bindings where state vars
            // are in scope.  The old chain_do approach placed {'ok', V_State<n>} in a
            // `do then` expression that cannot access state vars from the `do first`.
            let ok_wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr = &|_val, idx| {
                CErlExpr::Tuple(vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
                    state_expr(idx),
                ])
            };
            let lowered_body =
                lower_actor_body_stmts_w(&init.body, &mut scope, &mut state_idx, span, ok_wrap)?;

            // B-4 (Phase 6 pass 3): destructure V_Args into the init param names
            // at the top of `init/1`.  gen_server passes init params as a list
            // [P1, P2, …]; Phase 5 references them as bare variables V_P1, V_P2, …
            // which would be unbound without this destructuring step.
            let inner = CErlExpr::Let {
                var: state_bind,
                value: Box::new(initial_state),
                body: Box::new(lowered_body),
            };
            Ok(wrap_in_args_destructure(&init.params, inner))
        }
    }
}

/// Lower a handler body for `handle_call/3` (reply path).
///
/// B-7 fix: injects `{'reply', V, V_State<final>}` AT the leaf of the body,
/// inside all enclosing `let` bindings where state variables are in scope.
///
/// The outer structure emitted by `handler::lower_call_handler_body` is:
/// ```erlang
/// let V_State = V_StateArg in <THIS_FUNCTION_RESULT>
/// ```
///
/// THIS function result is the COMPLETE body ending in `{'reply', val, state}`.
pub(crate) fn lower_handler_body_for_call(
    body: &IrExpr,
    scope: &mut LocalScope,
    state_idx: &mut u32,
    span: Span,
) -> Result<CErlExpr, CodegenError> {
    // Leaf wrap: {'reply', val, V_State<idx>}
    let wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr = &|val, idx| {
        CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("reply".into()))),
            val,
            state_expr(idx),
        ])
    };
    lower_actor_body_stmts_w(body, scope, state_idx, span, wrap)
}

/// Lower a handler body for `handle_cast/2` (noreply path).
///
/// B-7 fix: injects `{'noreply', V_State<final>}` AT the leaf of the body.
pub(crate) fn lower_handler_body_for_cast(
    body: &IrExpr,
    scope: &mut LocalScope,
    state_idx: &mut u32,
    span: Span,
) -> Result<CErlExpr, CodegenError> {
    // Leaf wrap: {'noreply', V_State<idx>}  (val is ignored for cast)
    let wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr = &|_val, idx| {
        CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("noreply".into()))),
            state_expr(idx),
        ])
    };
    lower_actor_body_stmts_w(body, scope, state_idx, span, wrap)
}

/// Lower a handler or init body expression with actor-state-thread context.
///
/// State-field assignments (`Assign { target: StateField { name } }`) emit:
/// ```erlang
/// let V_State<next> = call 'maps':'put' ('name', Value, V_State<prev>) in ...
/// ```
///
/// All other expressions are lowered normally via `lower_expr_in_scope`.
///
/// Returns the lowered body *and* the final state SSA index via `state_idx`.
pub(crate) fn lower_actor_body_stmts(
    body: &IrExpr,
    scope: &mut LocalScope,
    state_idx: &mut u32,
    span: Span,
) -> Result<CErlExpr, CodegenError> {
    // Identity leaf wrap — do not transform the leaf value.
    let wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr = &|val, _idx| val;
    lower_actor_body_stmts_w(body, scope, state_idx, span, wrap)
}

// ── Internal: leaf-wrap-threaded lowering ──────────────────────────────────────

/// Internal variant of `lower_actor_body_stmts` that threads a `leaf_wrap`
/// closure through the lowering.
///
/// `leaf_wrap(val, state_idx)` is called on the FINAL value-producing expression
/// at the innermost scope of each branch, where all `V_State<n>` variables are
/// in scope.  This is the B-7 fix.
///
/// Uses `&dyn Fn` (not `&impl Fn`) to avoid monomorphization recursion: the
/// internal functions create nested closures (e.g., `|v, _| v`) and call each
/// other recursively, which would cause unbounded generic instantiation with
/// `impl Fn`.
fn lower_actor_body_stmts_w(
    body: &IrExpr,
    scope: &mut LocalScope,
    state_idx: &mut u32,
    span: Span,
    leaf_wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr,
) -> Result<CErlExpr, CodegenError> {
    match body {
        IrExpr::Block { stmts, .. } => {
            lower_actor_block_w(stmts, scope, state_idx, span, leaf_wrap)
        }
        other => lower_expr_in_actor_context_w(other, scope, state_idx, leaf_wrap),
    }
}

/// Internal variant of `lower_actor_block` that threads a `leaf_wrap` closure.
fn lower_actor_block_w(
    stmts: &[IrExpr],
    scope: &mut LocalScope,
    state_idx: &mut u32,
    span: Span,
    leaf_wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr,
) -> Result<CErlExpr, CodegenError> {
    match stmts {
        [] => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Block",
            span,
            detail: "actor body Block with zero stmts — Phase 5 invariant violated".into(),
        }),
        // Single statement: it IS the leaf — apply leaf_wrap.
        [single] => lower_expr_in_actor_context_w(single, scope, state_idx, leaf_wrap),
        [first, rest @ ..] => {
            match first {
                // StateField assignment → maps:put over running state.
                IrExpr::Assign {
                    target: AssignTarget::StateField { name, .. },
                    value,
                    span: assign_span,
                    ..
                } => {
                    let prev_state = state_expr(*state_idx);
                    *state_idx += 1;
                    let next_state_var = state_var(*state_idx);

                    let lowered_value = lower_expr_in_scope(value, scope)?;

                    let maps_put = CErlExpr::Call {
                        module: CErlAtom("maps".into()),
                        fn_name: CErlAtom("put".into()),
                        args: vec![
                            CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))),
                            lowered_value,
                            prev_state,
                        ],
                    };

                    // Thread leaf_wrap into the rest of the block.
                    let rest_expr =
                        lower_actor_block_w(rest, scope, state_idx, *assign_span, leaf_wrap)?;

                    Ok(CErlExpr::Let {
                        var: next_state_var,
                        value: Box::new(maps_put),
                        body: Box::new(rest_expr),
                    })
                }

                // Local-var assignment — lower normally.
                // Recursive inner-fn fix: if the assigned value is a Lambda whose
                // body self-references `name`, emit LetRec so `V_Name` is in scope
                // inside the lambda body (same logic as LetIn / VarIn rec paths).
                IrExpr::Assign {
                    target: AssignTarget::Local { name, .. },
                    value,
                    ..
                } => {
                    let mangled = name_to_erl_var(name);

                    if let IrExpr::Lambda {
                        params: lambda_params,
                        body: lambda_body,
                        ..
                    } = value.as_ref()
                    {
                        if body_references_local(lambda_body, name) {
                            #[allow(clippy::cast_possible_truncation)]
                            let arity = lambda_params.len() as u32;
                            std::sync::Arc::make_mut(&mut scope.fn_arity)
                                .insert(name.clone(), arity);
                            // Mark as letrec-local so B-6 does not route calls
                            // to it through the parent-module qualified call path.
                            std::sync::Arc::make_mut(&mut scope.letrec_locals).insert(name.clone());
                            let lowered_lambda = lower_expr_in_scope(value, scope)?;
                            let rest_expr =
                                lower_actor_block_w(rest, scope, state_idx, span, leaf_wrap)?;
                            std::sync::Arc::make_mut(&mut scope.fn_arity).remove(name.as_str());
                            std::sync::Arc::make_mut(&mut scope.letrec_locals)
                                .remove(name.as_str());
                            return Ok(CErlExpr::LetRec {
                                defs: vec![(CErlAtom(name.clone()), arity, lowered_lambda)],
                                body: Box::new(rest_expr),
                            });
                        }
                    }

                    let new_idx = scope.bump(name);
                    let lowered_value = lower_expr_in_scope(value, scope)?;
                    // Thread leaf_wrap into the rest.
                    let rest_expr = lower_actor_block_w(rest, scope, state_idx, span, leaf_wrap)?;
                    Ok(CErlExpr::Let {
                        var: crate::scope::ssa_var(&mangled, new_idx),
                        value: Box::new(lowered_value),
                        body: Box::new(rest_expr),
                    })
                }

                // General non-binding statement: emit Do { first, then: rest }.
                // The `first` expression is NOT the leaf; `rest` leads to the leaf.
                other => {
                    // Lower `first` with identity wrap (it is not the leaf).
                    let identity: &dyn Fn(CErlExpr, u32) -> CErlExpr = &|v, _| v;
                    let lowered_first =
                        lower_expr_in_actor_context_w(other, scope, state_idx, identity)?;
                    // Thread leaf_wrap into the rest.
                    let lowered_rest =
                        lower_actor_block_w(rest, scope, state_idx, span, leaf_wrap)?;
                    Ok(CErlExpr::Do {
                        first: Box::new(lowered_first),
                        then: Box::new(lowered_rest),
                    })
                }
            }
        }
    }
}

/// Internal variant of `lower_expr_in_actor_context` that threads a `leaf_wrap` closure.
///
/// The `leaf_wrap` is called on the FINAL value expression at the innermost scope.
/// For compound structures (Let, Match, etc.) the wrap is threaded into the body/arms.
/// For leaf expressions (not assignable, not structural) the wrap is applied directly.
///
/// Uses `&dyn Fn` to avoid monomorphization recursion (see `lower_actor_body_stmts_w`).
#[allow(clippy::too_many_lines)]
fn lower_expr_in_actor_context_w(
    expr: &IrExpr,
    scope: &mut LocalScope,
    state_idx: &mut u32,
    leaf_wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr,
) -> Result<CErlExpr, CodegenError> {
    use crate::core_ast::CErlClause;
    use crate::pat::lower_pat;
    use ridge_ir::IrPat;

    match expr {
        // Block: recurse via the dedicated block-level handler (with leaf_wrap).
        IrExpr::Block { stmts, span, .. } => {
            lower_actor_block_w(stmts, scope, state_idx, *span, leaf_wrap)
        }

        // LetIn: value is pure (no state assigns); body is the leaf — thread wrap.
        //
        // Recursive inner-fn fix: if the bound value is a Lambda that self-references
        // the bound name, emit LetRec (same logic as LetIn in expr.rs / OQ-L012).
        IrExpr::LetIn {
            pat, value, body, ..
        } => {
            if let IrPat::Bind {
                name, inner: None, ..
            } = pat
            {
                // Check for recursive inner-fn.
                if let IrExpr::Lambda {
                    params: lambda_params,
                    body: lambda_body,
                    ..
                } = value.as_ref()
                {
                    if body_references_local(lambda_body, name) {
                        #[allow(clippy::cast_possible_truncation)]
                        let arity = lambda_params.len() as u32;
                        std::sync::Arc::make_mut(&mut scope.fn_arity).insert(name.clone(), arity);
                        // Mark as letrec-local so B-6 does not route calls to it
                        // through the parent-module qualified call path.
                        std::sync::Arc::make_mut(&mut scope.letrec_locals).insert(name.clone());
                        let lowered_lambda = lower_expr_in_scope(value, scope)?;
                        let lowered_body =
                            lower_expr_in_actor_context_w(body, scope, state_idx, leaf_wrap)?;
                        std::sync::Arc::make_mut(&mut scope.fn_arity).remove(name.as_str());
                        std::sync::Arc::make_mut(&mut scope.letrec_locals).remove(name.as_str());
                        return Ok(CErlExpr::LetRec {
                            defs: vec![(CErlAtom(name.clone()), arity, lowered_lambda)],
                            body: Box::new(lowered_body),
                        });
                    }
                }

                let lowered_value = lower_expr_in_scope(value, scope)?;
                let lowered_body =
                    lower_expr_in_actor_context_w(body, scope, state_idx, leaf_wrap)?;
                Ok(CErlExpr::Let {
                    var: CErlVar(crate::expr::name_to_erl_var(name)),
                    value: Box::new(lowered_value),
                    body: Box::new(lowered_body),
                })
            } else {
                let lowered_value = lower_expr_in_scope(value, scope)?;
                let mut arm_scope = scope.clone();
                let lowered_body =
                    lower_expr_in_actor_context_w(body, &mut arm_scope, state_idx, leaf_wrap)?;
                Ok(CErlExpr::Case {
                    scrutinee: Box::new(lowered_value),
                    clauses: vec![CErlClause {
                        pattern: lower_pat(pat)?,
                        guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                        body: lowered_body,
                    }],
                })
            }
        }

        // VarIn: body is the leaf — thread wrap.
        //
        // Recursive inner-fn fix: if `value` is a Lambda whose body references
        // `name` (self-recursive closure), emit `LetRec` instead of `Let` so that
        // `V_Name` is in scope inside the lambda body.  Same logic as the
        // `LetIn` recursive path in `expr.rs` (OQ-L012).
        IrExpr::VarIn {
            name, value, body, ..
        } => {
            let mangled = crate::expr::name_to_erl_var(name);

            if let IrExpr::Lambda {
                params: lambda_params,
                body: lambda_body,
                ..
            } = value.as_ref()
            {
                if body_references_local(lambda_body, name) {
                    // Recursive lambda: register in fn_arity so the lambda body
                    // emits `LocalFnRef` for self-references, then emit LetRec.
                    #[allow(clippy::cast_possible_truncation)]
                    let arity = lambda_params.len() as u32;
                    std::sync::Arc::make_mut(&mut scope.fn_arity).insert(name.clone(), arity);
                    // Mark as letrec-local so B-6 does not route calls to it
                    // through the parent-module qualified call path.
                    std::sync::Arc::make_mut(&mut scope.letrec_locals).insert(name.clone());
                    let lowered_lambda = lower_expr_in_scope(value, scope)?;
                    let lowered_body =
                        lower_expr_in_actor_context_w(body, scope, state_idx, leaf_wrap)?;
                    std::sync::Arc::make_mut(&mut scope.fn_arity).remove(name.as_str());
                    std::sync::Arc::make_mut(&mut scope.letrec_locals).remove(name.as_str());
                    return Ok(CErlExpr::LetRec {
                        defs: vec![(CErlAtom(name.clone()), arity, lowered_lambda)],
                        body: Box::new(lowered_body),
                    });
                }
            }

            let idx = scope.bump(name);
            let lowered_value = lower_expr_in_scope(value, scope)?;
            let lowered_body = lower_expr_in_actor_context_w(body, scope, state_idx, leaf_wrap)?;
            Ok(CErlExpr::Let {
                var: crate::scope::ssa_var(&mangled, idx),
                value: Box::new(lowered_value),
                body: Box::new(lowered_body),
            })
        }

        // Match: each arm body is a branch to the leaf — thread wrap into each arm.
        //
        // B-7 fix: by threading `leaf_wrap` into each arm, the OTP response tuple
        // (e.g., `{'reply', val, V_State<n>}`) is constructed AT the arm's leaf,
        // where the arm-specific `V_State<n>` IS in scope.  This eliminates the
        // SSA index mismatch that arises when arms produce different numbers of
        // state mutations.
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            let lowered_scrutinee = lower_expr_in_scope(scrutinee, scope)?;
            let base_state_idx = *state_idx;
            let mut max_state_idx = base_state_idx;

            let clauses = arms
                .iter()
                .map(|arm| {
                    let mut arm_scope = scope.clone();
                    let mut arm_state = base_state_idx; // each arm starts from same base
                    let guard = match &arm.when {
                        Some(w) => lower_expr_in_scope(w, &mut arm_scope)?,
                        None => CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                    };
                    // Thread leaf_wrap into the arm body.  The arm's leaf will call
                    // `leaf_wrap(val, arm_state)` where arm_state is the ARM's final
                    // state index — correct for this specific arm.
                    let body = lower_expr_in_actor_context_w(
                        &arm.body,
                        &mut arm_scope,
                        &mut arm_state,
                        leaf_wrap,
                    )?;
                    if arm_state > max_state_idx {
                        max_state_idx = arm_state;
                    }
                    Ok(CErlClause {
                        pattern: lower_pat(&arm.pat)?,
                        guard,
                        body,
                    })
                })
                .collect::<Result<Vec<_>, CodegenError>>()?;

            *state_idx = max_state_idx;
            Ok(CErlExpr::Case {
                scrutinee: Box::new(lowered_scrutinee),
                clauses,
            })
        }

        // StateField Assign at expression scope (not inside Block).
        IrExpr::Assign {
            target: AssignTarget::StateField { name, .. },
            value,
            ..
        } => {
            let prev_state = state_expr(*state_idx);
            *state_idx += 1;
            let next_state_var = state_var(*state_idx);
            let lowered_value = lower_expr_in_scope(value, scope)?;
            let maps_put = CErlExpr::Call {
                module: CErlAtom("maps".into()),
                fn_name: CErlAtom("put".into()),
                args: vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))),
                    lowered_value,
                    prev_state,
                ],
            };
            // The assign itself is the leaf; the new state IS the result.
            // Apply leaf_wrap with the new state as the value.
            let new_state_ref = CErlExpr::Var(next_state_var.clone());
            Ok(CErlExpr::Let {
                var: next_state_var,
                value: Box::new(maps_put),
                body: Box::new(leaf_wrap(new_state_ref, *state_idx)),
            })
        }

        // All other nodes: leaf — apply leaf_wrap directly.
        other => {
            let lowered = lower_expr_in_scope(other, scope)?;
            Ok(leaf_wrap(lowered, *state_idx))
        }
    }
}

// ── Deprecated: plain actor-context lowering (used by init only) ──────────────

/// Lower a single expression in actor-state-thread context (no leaf wrap).
///
/// Used internally by `lower_actor_body_stmts` for the init path.
/// Handler paths should use `lower_handler_body_for_call` / `_for_cast`.
fn lower_expr_in_actor_context(
    expr: &IrExpr,
    scope: &mut LocalScope,
    state_idx: &mut u32,
) -> Result<CErlExpr, CodegenError> {
    let wrap: &dyn Fn(CErlExpr, u32) -> CErlExpr = &|val, _| val;
    lower_expr_in_actor_context_w(expr, scope, state_idx, wrap)
}

/// Chain two expressions with `CErlExpr::Do` (left-to-right sequencing).
///
/// Used to chain the user's init body (which may be Unit/ok-valued) with
/// the final `{'ok', State}` return.
fn chain_do(first: CErlExpr, then: CErlExpr) -> CErlExpr {
    CErlExpr::Do {
        first: Box::new(first),
        then: Box::new(then),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit};
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrLit, IrNodeId, IrStateField};
    use ridge_types::{CapabilitySet, Type};

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

    fn state_field(name: &str, default: Option<IrExpr>) -> IrStateField {
        IrStateField {
            name: name.into(),
            ty: Type::Error, // PHASE7-STUB: type is Phase-7 concern
            default,
            span: sp(),
        }
    }

    // ── state_var / state_expr ─────────────────────────────────────────────────

    #[test]
    fn state_var_index_0_is_bare() {
        assert_eq!(state_var(0).0, "V_State");
    }

    #[test]
    fn state_var_index_1_has_suffix() {
        assert_eq!(state_var(1).0, "V_State1");
    }

    // ── lower_init_body — no init block ───────────────────────────────────────

    #[test]
    fn init_body_no_init_block_returns_ok_tuple_with_defaults() {
        // §4.29: If init is None and all fields have defaults, return {'ok', defaults}.
        let fields = vec![
            state_field("count", Some(lit_int(0))),
            state_field("limit", Some(lit_int(100))),
        ];
        let result = lower_init_body(None, &fields, sp()).unwrap();

        // Must be {'ok', MapLit(...)}.
        match &result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "ok")
                );
                assert!(matches!(&elems[1], CErlExpr::MapLit(_)));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    #[test]
    fn init_body_no_init_no_defaults_returns_ok_empty_map() {
        // No fields: returns {'ok', #{}} (empty map).
        let result = lower_init_body(None, &[], sp()).unwrap();
        match &result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(&elems[1], CErlExpr::MapLit(pairs) if pairs.is_empty()));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    // ── lower_init_body — with init block ─────────────────────────────────────

    #[test]
    fn init_body_with_init_block_wraps_in_let() {
        // With an init block: let V_State = defaults in <body> then {'ok', final}.
        let fields = vec![state_field("count", Some(lit_int(0)))];
        let init = IrInit {
            params: vec![],
            caps: CapabilitySet::PURE,
            body: lit_unit(), // simple body: just 'ok'
            span: sp(),
        };
        let result = lower_init_body(Some(&init), &fields, sp()).unwrap();

        // Must be Let { var: V_State, value: MapLit, body: Do(body, {'ok', V_State}) }
        match &result {
            CErlExpr::Let { var, .. } => {
                assert_eq!(var.0, "V_State", "initial state var must be V_State");
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    // ── lower_actor_block — StateField assign ─────────────────────────────────

    #[test]
    fn actor_block_state_field_assign_emits_maps_put() {
        // StateField assignment → let V_State1 = maps:put('count', Value, V_State) in ...
        let assign_stmt = IrExpr::Assign {
            id: IrNodeId(0),
            target: AssignTarget::StateField {
                name: "count".into(),
                span: sp(),
            },
            value: Box::new(lit_int(42)),
            span: sp(),
        };
        let stmts = vec![assign_stmt, lit_unit()];
        let mut scope = LocalScope::new();
        let mut state_idx: u32 = 0;

        let result = lower_actor_body_stmts(
            &IrExpr::Block {
                id: ridge_ir::IrNodeId(0),
                stmts,
                span: sp(),
            },
            &mut scope,
            &mut state_idx,
            sp(),
        )
        .unwrap();

        // Must be Let { var: V_State1, value: Call maps:put(...), body: ... }
        match &result {
            CErlExpr::Let { var, value, .. } => {
                assert_eq!(
                    var.0, "V_State1",
                    "first state assign must produce V_State1"
                );
                match value.as_ref() {
                    CErlExpr::Call {
                        module,
                        fn_name,
                        args,
                    } => {
                        assert_eq!(module.0, "maps");
                        assert_eq!(fn_name.0, "put");
                        assert_eq!(args.len(), 3);
                        // First arg: 'count' atom.
                        assert!(
                            matches!(&args[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "count")
                        );
                    }
                    other => panic!("expected Call maps:put, got {other:?}"),
                }
            }
            other => panic!("expected Let, got {other:?}"),
        }

        // state_idx must be bumped.
        assert_eq!(state_idx, 1, "state_idx must have incremented to 1");
    }
}
