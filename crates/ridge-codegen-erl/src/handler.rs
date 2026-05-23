//! §4.30 — Lower `IrHandler` to `gen_server` callback clauses.
//!
//! Each `IrHandler` is emitted as a clause in **both** `handle_call/3` (for `?>`
//! callers) and `handle_cast/2` (for `!` callers) per resolved **OQ-E005** (§8.2).
//! This is because `!` vs `?>` is a call-site decision; the handler shape does not
//! constrain which direction a caller uses.  BEAM cost is one extra clause per handler.
//!
//! ## `handle_call/3` clause shape
//!
//! ```erlang
//! case Msg of
//!     {handler_tag, P1, ..., PN} when 'true' ->
//!         let V_State = State in
//!             <body ending in {'reply', val, V_State_n} at EACH LEAF>
//! end
//! ```
//!
//! ## `handle_cast/2` clause shape
//!
//! ```erlang
//! case Msg of
//!     {handler_tag, P1, ..., PN} when 'true' ->
//!         let V_State = State in
//!             <body ending in {'noreply', V_State_n} at EACH LEAF>
//! end
//! ```
//!
//! ## B-7 fix — state-varying Match arms
//!
//! The OTP response tuple is NOT added after the body — it is injected at the
//! innermost leaf of each arm via `leaf_wrap` threading.  This ensures that
//! arm-specific `V_State<n>` variables are always in scope when referenced.
//!
//! ## State threading (§3.12 + §4.8)
//!
//! The state map enters the handler as `V_State` (from the `State` argument).
//! Any `Assign { target: StateField }` in the body bumps the state SSA index and
//! emits `maps:put`.  The final state variable is returned in the tuple.

// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// Handler lowering functions are exercised via actor.rs's tests and through the
// actor module pipeline.  dead_code fires because lower_actor (their sole
// non-test caller) is itself only reachable from the actor module pipeline.
#![allow(dead_code)]

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlLit, CErlPat, CErlVar};
use crate::error::CodegenError;
use crate::expr::name_to_erl_var;
use crate::init::{lower_handler_body_for_call, lower_handler_body_for_cast, state_var};
use crate::scope::LocalScope;
use ridge_ir::IrHandler;
use ridge_resolve::ModuleId;
use rustc_hash::FxHashMap;

/// The BEAM argument name for the caller `From` in `handle_call/3`.
const FROM_VAR: &str = "_V_From";
/// The BEAM argument name for the incoming message in both callbacks.
const MSG_VAR: &str = "V_Msg";
/// The BEAM argument name for the current state map.
const STATE_ARG_VAR: &str = "V_StateArg";

// ── handle_call clause (§4.30) ────────────────────────────────────────────────

/// Build a `CErlClause` for `handle_call/3` from a single `IrHandler`.
///
/// Emits:
/// ```erlang
/// {'handler_tag', P1, ..., PN} when 'true' ->
///     let V_State = V_StateArg in
///         <body ending in {'reply', val, V_State_n} at each leaf>
/// ```
///
/// Per OQ-E005 (§8.2): all handlers appear in `handle_call/3` regardless of
/// their return type, because `!` vs `?>` is a call-site decision.
///
/// `parent_module_id` and `parent_beam_name` are used to emit qualified inter-
/// module calls for `SymbolRef::Local { module: parent_id }` references that
/// cross from an actor body into the parent BEAM module (B-6 fix).
// OQ-E005: emit in both handle_call and handle_cast (plan §4.30 + §8.2).
pub(crate) fn lower_handler_call_clause(
    handler: &IrHandler,
    fn_arity: &FxHashMap<String, u32>,
    parent_module_id: ModuleId,
    parent_beam_name: &str,
) -> Result<CErlClause, CodegenError> {
    let mut scope =
        LocalScope::with_actor_parent(fn_arity.clone(), parent_module_id, parent_beam_name);
    let mut state_idx: u32 = 0;

    // Pattern: {handler_tag, P1, ..., PN}
    let pattern = build_handler_pattern(&handler.message_name, &handler.params);

    // Body:
    //   let V_State = V_StateArg in
    //       let V_Reply = <handler_body> in
    //           {'reply', V_Reply, V_State_final}
    let body = lower_call_handler_body(handler, &mut scope, &mut state_idx)?;

    Ok(CErlClause {
        pattern,
        guard: lit_true(),
        body,
    })
}

// ── handle_cast clause (§4.30) ────────────────────────────────────────────────

/// Build a `CErlClause` for `handle_cast/2` from a single `IrHandler`.
///
/// Emits:
/// ```erlang
/// {'handler_tag', P1, ..., PN} when 'true' ->
///     let V_State = V_StateArg in
///         <body ending in {'noreply', V_State_n} at each leaf>
/// ```
///
/// Per OQ-E005 (§8.2): all handlers appear in `handle_cast/2` regardless of
/// their return type, because `!` vs `?>` is a call-site decision.
///
/// `parent_module_id` and `parent_beam_name` are used to emit qualified inter-
/// module calls for `SymbolRef::Local { module: parent_id }` references that
/// cross from an actor body into the parent BEAM module (B-6 fix).
// OQ-E005: emit in both handle_call and handle_cast (plan §4.30 + §8.2).
pub(crate) fn lower_handler_cast_clause(
    handler: &IrHandler,
    fn_arity: &FxHashMap<String, u32>,
    parent_module_id: ModuleId,
    parent_beam_name: &str,
) -> Result<CErlClause, CodegenError> {
    let mut scope =
        LocalScope::with_actor_parent(fn_arity.clone(), parent_module_id, parent_beam_name);
    let mut state_idx: u32 = 0;

    // Pattern: {handler_tag, P1, ..., PN}
    let pattern = build_handler_pattern(&handler.message_name, &handler.params);

    // Body:
    //   let V_State = V_StateArg in
    //       let _V_Reply = <handler_body> in
    //           {'noreply', V_State_final}
    let body = lower_cast_handler_body(handler, &mut scope, &mut state_idx)?;

    Ok(CErlClause {
        pattern,
        guard: lit_true(),
        body,
    })
}

// ── Helper: handler body lowering ─────────────────────────────────────────────

/// Lower the handler body for `handle_call/3` (reply path).
///
/// Shape:
/// ```erlang
/// let V_State = V_StateArg in
///     <B-7 leaf-wrapped body ending in {'reply', val, V_State_final}>
/// ```
///
/// B-7 fix: the `{'reply', val, V_State<n>}` tuple is constructed at the LEAF of
/// each arm by `lower_handler_body_for_call`, so arm-specific state variables are
/// always in scope.
fn lower_call_handler_body(
    handler: &IrHandler,
    scope: &mut LocalScope,
    state_idx: &mut u32,
) -> Result<CErlExpr, CodegenError> {
    // lower_handler_body_for_call produces the complete body ending in
    // {'reply', val, V_State<final>} at each leaf.
    let full_body = lower_handler_body_for_call(&handler.body, scope, state_idx, handler.span)?;

    // let V_State = V_StateArg in <full_body>
    Ok(CErlExpr::Let {
        var: state_var(0),
        value: Box::new(CErlExpr::Var(CErlVar(STATE_ARG_VAR.into()))),
        body: Box::new(full_body),
    })
}

/// Lower the handler body for `handle_cast/2` (noreply path).
///
/// Shape:
/// ```erlang
/// let V_State = V_StateArg in
///     <B-7 leaf-wrapped body ending in {'noreply', V_State_final}>
/// ```
///
/// B-7 fix: the `{'noreply', V_State<n>}` tuple is constructed at the LEAF of
/// each arm by `lower_handler_body_for_cast`, so arm-specific state variables are
/// always in scope.
fn lower_cast_handler_body(
    handler: &IrHandler,
    scope: &mut LocalScope,
    state_idx: &mut u32,
) -> Result<CErlExpr, CodegenError> {
    // lower_handler_body_for_cast produces the complete body ending in
    // {'noreply', V_State<final>} at each leaf.
    let full_body = lower_handler_body_for_cast(&handler.body, scope, state_idx, handler.span)?;

    // let V_State = V_StateArg in <full_body>
    Ok(CErlExpr::Let {
        var: state_var(0),
        value: Box::new(CErlExpr::Var(CErlVar(STATE_ARG_VAR.into()))),
        body: Box::new(full_body),
    })
}

// ── Helper: message pattern ───────────────────────────────────────────────────

/// Build the `{handler_tag, P1, ..., PN}` pattern for a handler clause.
///
/// Handler parameters are mangled to Erlang variable names via `name_to_erl_var`.
fn build_handler_pattern(message_name: &str, params: &[ridge_ir::IrParam]) -> CErlPat {
    let mut pats = Vec::with_capacity(params.len() + 1);
    // Tag: atom for the handler name.
    pats.push(CErlPat::Lit(CErlLit::Atom(CErlAtom(
        message_name.to_owned(),
    ))));
    // Parameters: one Var per param.
    for p in params {
        pats.push(CErlPat::Var(CErlVar(name_to_erl_var(&p.name))));
    }
    CErlPat::Tuple(pats)
}

/// `'true'` atom expression — used as the default guard for `case` clauses.
fn lit_true() -> CErlExpr {
    CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into())))
}

/// Produce the function signature variable list for a callback.
///
/// - `handle_call/3` → `[V_Msg, _V_From, V_StateArg]`
/// - `handle_cast/2` → `[V_Msg, V_StateArg]`
pub(crate) fn call_params() -> Vec<CErlVar> {
    vec![
        CErlVar(MSG_VAR.into()),
        CErlVar(FROM_VAR.into()),
        CErlVar(STATE_ARG_VAR.into()),
    ]
}

pub(crate) fn cast_params() -> Vec<CErlVar> {
    vec![CErlVar(MSG_VAR.into()), CErlVar(STATE_ARG_VAR.into())]
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit, CErlPat};
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrHandler, IrLit, IrNodeId, IrParam};
    use ridge_resolve::{ModuleId, NodeId};
    use ridge_types::{CapabilitySet, Type};

    /// Dummy parent module id/name for tests that don't exercise B-6 cross-module logic.
    fn no_parent() -> (ModuleId, &'static str) {
        (ModuleId(0), "ridge_test_parent")
    }

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

    fn make_handler(name: &str, params: Vec<IrParam>, body: IrExpr) -> IrHandler {
        IrHandler {
            message_name: name.into(),
            params,
            ret_ty: Type::Error, // PHASE7-STUB
            caps: CapabilitySet::PURE,
            body,
            origin: NodeId(0),
            span: sp(),
            doc: None,
        }
    }

    fn make_param(name: &str) -> IrParam {
        IrParam {
            name: name.into(),
            ty: Type::Error, // PHASE7-STUB
            span: sp(),
        }
    }

    // §4.30 — handle_call clause tests

    #[test]
    fn call_clause_pattern_has_handler_tag() {
        let handler = make_handler("increment", vec![make_param("amount")], lit_unit());
        let (pid, pbm) = no_parent();
        let clause = lower_handler_call_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();

        // Pattern must be a Tuple starting with the handler name atom.
        match &clause.pattern {
            CErlPat::Tuple(pats) => {
                assert_eq!(pats.len(), 2, "tag + 1 param");
                assert!(
                    matches!(&pats[0], CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "increment"),
                    "first pattern element must be the handler tag 'increment'"
                );
            }
            other => panic!("expected Tuple pattern, got {other:?}"),
        }
    }

    #[test]
    fn call_clause_zero_params_tuple_has_only_tag() {
        let handler = make_handler("reset", vec![], lit_unit());
        let (pid, pbm) = no_parent();
        let clause = lower_handler_call_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();

        match &clause.pattern {
            CErlPat::Tuple(pats) => {
                assert_eq!(pats.len(), 1, "zero params: only the tag");
            }
            other => panic!("expected Tuple pattern, got {other:?}"),
        }
    }

    #[test]
    fn call_clause_guard_is_true() {
        let handler = make_handler("get", vec![], lit_unit());
        let (pid, pbm) = no_parent();
        let clause = lower_handler_call_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();
        assert!(
            matches!(&clause.guard, CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "true"),
            "guard must be 'true'"
        );
    }

    #[test]
    fn call_clause_body_starts_with_let_state() {
        // Body must start: let V_State = V_StateArg in ...
        let handler = make_handler("increment", vec![], lit_unit());
        let (pid, pbm) = no_parent();
        let clause = lower_handler_call_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();

        match &clause.body {
            CErlExpr::Let { var, value, .. } => {
                assert_eq!(var.0, "V_State", "handler body must bind V_State first");
                assert!(
                    matches!(value.as_ref(), CErlExpr::Var(CErlVar(s)) if s == STATE_ARG_VAR),
                    "V_State must be bound to V_StateArg"
                );
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    // §4.30 — handle_cast clause tests

    #[test]
    fn cast_clause_pattern_matches_call_clause_pattern() {
        let handler = make_handler("send_event", vec![make_param("event")], lit_unit());
        let (pid, pbm) = no_parent();
        let call_clause =
            lower_handler_call_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();
        let cast_clause =
            lower_handler_cast_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();

        // Both clauses must have the same pattern structure.
        match (&call_clause.pattern, &cast_clause.pattern) {
            (CErlPat::Tuple(call_pats), CErlPat::Tuple(cast_pats)) => {
                assert_eq!(
                    call_pats.len(),
                    cast_pats.len(),
                    "call and cast must have same pattern length"
                );
            }
            other => panic!("unexpected pattern shape: {other:?}"),
        }
    }

    #[test]
    #[allow(clippy::items_after_statements)]
    fn cast_clause_body_contains_noreply() {
        let handler = make_handler("reset", vec![], lit_unit());
        let (pid, pbm) = no_parent();
        let clause = lower_handler_cast_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();

        // Walk down to find the noreply tuple. Skip past Let and Do — the cast
        // leaf wrap is now `Do { first: <leaf side effect>, then: <noreply> }`
        // so the noreply tuple lives in the `then` arm.
        fn contains_noreply(expr: &CErlExpr) -> bool {
            match expr {
                CErlExpr::Tuple(elems) => {
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "noreply")
                }
                CErlExpr::Let { body, .. } => contains_noreply(body),
                CErlExpr::Do { then, .. } => contains_noreply(then),
                _ => false,
            }
        }

        assert!(
            contains_noreply(&clause.body),
            "cast clause body must contain a noreply tuple"
        );
    }

    /// Regression: the cast leaf must sequence the body value for its side effects
    /// before returning `{noreply, V_State}`. A naive `|_, idx| {noreply, ...}` leaf
    /// wrap discards every Io call / message send inside a `!`-invoked handler.
    #[test]
    #[allow(clippy::items_after_statements)]
    fn cast_clause_preserves_leaf_side_effect() {
        // Handler body: a Call node (stands in for any side-effecting leaf).
        let leaf_call = IrExpr::Call {
            id: IrNodeId(0),
            callee: Box::new(lit_unit()),
            args: vec![],
            span: sp(),
        };
        let handler = make_handler("ping", vec![], leaf_call);
        let (pid, pbm) = no_parent();
        let clause = lower_handler_cast_clause(&handler, &FxHashMap::default(), pid, pbm).unwrap();

        // The body must contain a Do node whose `then` reaches the noreply tuple;
        // if it doesn't, the leaf side effect would be silently dropped.
        fn has_do_before_noreply(expr: &CErlExpr) -> bool {
            match expr {
                CErlExpr::Do { then, .. } => matches!(then.as_ref(),
                    CErlExpr::Tuple(elems)
                        if matches!(&elems[0],
                            CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "noreply")),
                CErlExpr::Let { body, .. } => has_do_before_noreply(body),
                _ => false,
            }
        }

        assert!(
            has_do_before_noreply(&clause.body),
            "cast clause must sequence the leaf side effect before the noreply tuple, \
             got: {:#?}",
            clause.body
        );
    }

    // call_params / cast_params

    #[test]
    fn call_params_has_three_args() {
        assert_eq!(call_params().len(), 3, "handle_call/3 has 3 params");
    }

    #[test]
    fn cast_params_has_two_args() {
        assert_eq!(cast_params().len(), 2, "handle_cast/2 has 2 params");
    }
}
