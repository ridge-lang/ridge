//! §4.9 — `IrExpr::Return` lowering utilities.
//!
//! Three responsibilities:
//!
//! 1. [`lower_return`] — emit the throw form for a non-tail `Return`.
//! 2. [`has_non_tail_return`] — walk an IR body to detect any `Return` that is
//!    *not* in tail position of its enclosing fn.
//! 3. [`wrap_with_return_catch`] — wrap a lowered body in a `try/catch` that
//!    catches the `{ridge_return, V}` throw.
//! 4. [`elide_tail_returns`] — rewrite tail-position `Return { value }` nodes
//!    to just `*value` (returns an owned clone of the IR).
//! 5. [`lower_fn_body`] — integration point called by T8 item-level emission.
//!    Routes through elide/wrap based on `has_non_tail_return`.

// T4 helpers consumed by expr.rs and T8.  Until T8 wires the top-level
// pipeline these items are only exercised from the test suite.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it for explicitness per plan §2.2.
#![allow(clippy::redundant_pub_crate)]
// lower_fn_body / wrap_with_return_catch contain non-trivial match nesting;
// suppress the line-count lint for the whole file.
#![allow(clippy::too_many_lines)]

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlLit, CErlPat, CErlVar};
use crate::error::CodegenError;
use crate::expr::lower_expr_in_scope;
use crate::scope::LocalScope;
use ridge_ir::IrExpr;

// ── 1. lower_return ───────────────────────────────────────────────────────────

/// Emit the throw form for a non-tail `Return` (§4.9).
///
/// Always emits:
/// ```erlang
/// call 'erlang':'throw' ({ridge_return, Value})
/// ```
///
/// This is the BEAM-canonical translation for non-local returns.  The enclosing
/// fn body must be wrapped in [`wrap_with_return_catch`] to catch this throw.
pub(crate) fn lower_return(value: CErlExpr) -> CErlExpr {
    CErlExpr::Call {
        module: CErlAtom("erlang".into()),
        fn_name: CErlAtom("throw".into()),
        args: vec![CErlExpr::Tuple(vec![
            CErlExpr::Lit(CErlLit::Atom(CErlAtom("ridge_return".into()))),
            value,
        ])],
    }
}

// ── 2. has_non_tail_return ────────────────────────────────────────────────────

/// Walk `body` and return `true` iff any [`IrExpr::Return`] appears **outside**
/// tail position of its enclosing fn (§4.9).
///
/// Tail positions (relative to the fn body):
/// - The body expression itself.
/// - The last stmt of a `Block` that is itself in tail position.
/// - Every arm body of a `Match` that is itself in tail position.
/// - The `body` (continuation) of a `LetIn`/`VarIn` that is itself in tail
///   position.
///
/// Lambdas are **opaque**: we do NOT recurse into `IrExpr::Lambda`.  A `Return`
/// inside a lambda body belongs to the lambda's own try/catch frame, not the
/// outer fn's.
pub(crate) fn has_non_tail_return(body: &IrExpr) -> bool {
    has_non_tail_return_inner(body, /* in_tail = */ true)
}

/// Inner recursive walker.  `in_tail` is `true` when we are currently visiting
/// a tail-position node.
fn has_non_tail_return_inner(expr: &IrExpr, in_tail: bool) -> bool {
    match expr {
        // A Return at this node.
        IrExpr::Return { .. } => {
            // Non-tail Return detected.
            !in_tail
        }

        // Block: all stmts except the last are non-tail; the last is tail iff
        // the block itself is in tail position.
        IrExpr::Block { stmts, .. } => {
            let n = stmts.len();
            for (i, stmt) in stmts.iter().enumerate() {
                let stmt_is_tail = in_tail && (i == n - 1);
                if has_non_tail_return_inner(stmt, stmt_is_tail) {
                    return true;
                }
            }
            false
        }

        // Match: the scrutinee is always non-tail; each arm body is tail iff
        // the match itself is in tail position.
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            if has_non_tail_return_inner(scrutinee, false) {
                return true;
            }
            for arm in arms {
                if let Some(guard) = &arm.when {
                    if has_non_tail_return_inner(guard, false) {
                        return true;
                    }
                }
                if has_non_tail_return_inner(&arm.body, in_tail) {
                    return true;
                }
            }
            false
        }

        // LetIn / VarIn: the value is non-tail; the body (continuation)
        // inherits the tail-position flag.
        IrExpr::LetIn { value, body, .. } | IrExpr::VarIn { value, body, .. } => {
            has_non_tail_return_inner(value, false) || has_non_tail_return_inner(body, in_tail)
        }

        // For all other variants, conservatively walk into sub-expressions
        // as non-tail positions (they cannot be a tail Return of the outer fn).
        IrExpr::Assign { value, .. } => has_non_tail_return_inner(value, false),

        IrExpr::Call { callee, args, .. } => {
            has_non_tail_return_inner(callee, false)
                || args.iter().any(|a| has_non_tail_return_inner(a, false))
        }

        IrExpr::Construct { fields, .. } => fields
            .iter()
            .any(|(_, v)| has_non_tail_return_inner(v, false)),

        IrExpr::Field { base, .. } => has_non_tail_return_inner(base, false),

        IrExpr::Tuple { elems, .. } | IrExpr::ListLit { elems, .. } => {
            elems.iter().any(|e| has_non_tail_return_inner(e, false))
        }

        IrExpr::Cons { head, tail, .. } => {
            has_non_tail_return_inner(head, false) || has_non_tail_return_inner(tail, false)
        }

        IrExpr::Send { handle, args, .. } | IrExpr::Ask { handle, args, .. } => {
            has_non_tail_return_inner(handle, false)
                || args.iter().any(|a| has_non_tail_return_inner(a, false))
        }

        IrExpr::Spawn { args, .. } => args.iter().any(|a| has_non_tail_return_inner(a, false)),

        // Atoms (Lit, Local, Symbol) and future variants: no sub-expressions
        // that can be Return.
        _ => false,
    }
}

// ── 3. wrap_with_return_catch ─────────────────────────────────────────────────

/// Wrap `body` in a `try/catch` that intercepts `{ridge_return, V}` throws
/// (§4.9).
///
/// ## erlc `+from_core` try/catch constraints
///
/// The `+from_core` parser imposes strict constraints on `try` expression
/// syntax that differ from the printed Core Erlang format the compiler itself
/// produces with `+to_core`:
///
/// - **`of` clauses**: `<Pattern> ->` only — NO `when` guard is allowed.
/// - **`catch` clauses**: MUST use exactly three bare variables
///   `<V_Class, V_Value, V_Stk> ->` — literal atoms or tuple patterns in the
///   catch head are rejected.  Pattern dispatch must be done inside the
///   catch body via an ordinary `case` expression.
/// - **No trailing `end`**: `try … of … catch …` must NOT be followed by `end`.
///   A trailing `end` causes "syntax error before: 'end'" when the catch body
///   ends with `case … end` (two consecutive `end` keywords confuse the parser).
///
/// Emitted shape (no trailing `end`):
/// ```erlang
/// try Body
/// of <V_Result> ->
///   V_Result
/// catch <V_ExcClass, V_ExcValue, V_ExcStk> ->
///   case V_ExcClass of
///     <'throw'> when 'true' ->
///       case V_ExcValue of
///         <{'ridge_return', V_Return}> when 'true' -> V_Return
///         <V_OtherErr> when 'true' ->
///           call 'erlang':'raise' ('throw', V_OtherErr, V_ExcStk)
///       end
///     <V_OtherClass> when 'true' ->
///       call 'erlang':'raise' (V_OtherClass, V_ExcValue, V_ExcStk)
///   end
/// ```
///
/// The `CErlClause` wrapper for the `of` clause still carries a `guard` field
/// (set to `Lit(Atom("true"))`) so the in-memory AST is consistent; the printer
/// skips `when` for `of` clauses.
///
/// The catch clause uses a sentinel `CErlPat::Tuple` with exactly three
/// `CErlPat::Var` elements for the three catch variables.  The printer emits
/// them as `<V_ExcClass, V_ExcValue, V_ExcStk>` (comma-separated, no wrapping
/// `{}`, no `when` guard).
///
/// The two nested `case` expressions in the catch body are required to:
/// 1. Match `throw:{ridge_return, V}` and return `V`.
/// 2. Re-raise any other exception via `erlang:raise/3` (fallback clause).
///    This exhaustiveness is required by erlc's internal consistency check
///    (`"ambiguous_catch_try_state"`).
pub(crate) fn wrap_with_return_catch(body: CErlExpr) -> CErlExpr {
    // Catch body — two nested case expressions, matching the `+from_core`
    // constraints:
    //
    //   case V_ExcClass of
    //     <'throw'> when 'true' ->
    //       case V_ExcValue of
    //         <{'ridge_return', V_Return}> when 'true' -> V_Return
    //         <V_OtherErr> when 'true' ->
    //           primop 'raise' (V_ExcStk, V_OtherErr)
    //       end
    //     <V_OtherClass> when 'true' ->
    //       primop 'raise' (V_ExcStk, V_ExcValue)
    //   end
    //
    // Why nested case instead of `case <V_ExcClass, V_ExcValue> of ...`:
    //   The ANF pass would lift a tuple-scrutinee `{V_ExcClass, V_ExcValue}`
    //   into a `let V_Anf = {…} in case V_Anf of`, creating a `let…in` inside
    //   the catch body.  `+from_core` cannot parse `let…in` followed by `end`
    //   (as the second `end` — from the try — collides with the first `end`
    //   from the let-case).  Nested single-scrutinee cases avoid this entirely.
    //
    // Why a fallback rethrow clause:
    //   erlc's internal consistency check ("ambiguous_catch_try_state") fires
    //   if a catch body has a `case` expression that doesn't exhaustively
    //   handle all exceptions (i.e. only one clause).  A wildcard rethrow
    //   satisfies the exhaustiveness requirement.
    // call 'erlang':'raise' (Class, Value, Stacktrace) — re-raise helper.
    // Using the standard BIF rather than `primop 'raise'` because `primop` is
    // not a CErlExpr variant and `erlang:raise/3` is accepted by erlc +from_core.
    let erlang_raise = |class: CErlExpr, value: CErlExpr, stk: CErlExpr| -> CErlExpr {
        CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("raise".into()),
            args: vec![class, value, stk],
        }
    };

    let inner_case = CErlExpr::Case {
        scrutinee: Box::new(CErlExpr::Var(CErlVar("V_ExcValue".into()))),
        clauses: vec![
            // <{'ridge_return', V_Return}> when 'true' -> V_Return
            CErlClause {
                pattern: CErlPat::Tuple(vec![
                    CErlPat::Lit(CErlLit::Atom(CErlAtom("ridge_return".into()))),
                    CErlPat::Var(CErlVar("V_Return".into())),
                ]),
                guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                body: CErlExpr::Var(CErlVar("V_Return".into())),
            },
            // <V_OtherErr> when 'true' ->
            //   call 'erlang':'raise' ('throw', V_OtherErr, V_ExcStk)
            CErlClause {
                pattern: CErlPat::Var(CErlVar("V_OtherErr".into())),
                guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                body: erlang_raise(
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("throw".into()))),
                    CErlExpr::Var(CErlVar("V_OtherErr".into())),
                    CErlExpr::Var(CErlVar("V_ExcStk".into())),
                ),
            },
        ],
    };

    let catch_body = CErlExpr::Case {
        scrutinee: Box::new(CErlExpr::Var(CErlVar("V_ExcClass".into()))),
        clauses: vec![
            // <'throw'> when 'true' -> inner_case
            CErlClause {
                pattern: CErlPat::Lit(CErlLit::Atom(CErlAtom("throw".into()))),
                guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                body: inner_case,
            },
            // <V_OtherClass> when 'true' ->
            //   call 'erlang':'raise' (V_OtherClass, V_ExcValue, V_ExcStk)
            CErlClause {
                pattern: CErlPat::Var(CErlVar("V_OtherClass".into())),
                guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                body: erlang_raise(
                    CErlExpr::Var(CErlVar("V_OtherClass".into())),
                    CErlExpr::Var(CErlVar("V_ExcValue".into())),
                    CErlExpr::Var(CErlVar("V_ExcStk".into())),
                ),
            },
        ],
    };

    CErlExpr::Try {
        body: Box::new(body),
        // `of` clause: `<V_Result> -> V_Result` (printer emits without `when`).
        of: vec![CErlClause {
            pattern: CErlPat::Var(CErlVar("V_Result".into())),
            // Guard is stored for AST completeness; printer skips it for `of` clauses.
            guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
            body: CErlExpr::Var(CErlVar("V_Result".into())),
        }],
        // `catch` clause: three bare variables; dispatch via nested case in body.
        // Printer emits: `<V_ExcClass, V_ExcValue, V_ExcStk> -> case ...`
        catch: vec![CErlClause {
            // Sentinel: 3-element tuple of Var — printer unpacks as
            // `<V_ExcClass, V_ExcValue, V_ExcStk>`.
            pattern: CErlPat::Tuple(vec![
                CErlPat::Var(CErlVar("V_ExcClass".into())),
                CErlPat::Var(CErlVar("V_ExcValue".into())),
                CErlPat::Var(CErlVar("V_ExcStk".into())),
            ]),
            // Guard is ignored by printer for catch clauses.
            guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
            body: catch_body,
        }],
    }
}

// ── 4. elide_tail_returns ─────────────────────────────────────────────────────

/// Rewrite tail-position `Return { value }` nodes to just `*value`.
///
/// Used when `has_non_tail_return` is `false` — all `Return` nodes are in tail
/// position, so they can be stripped without emitting throw+catch.  Clones the
/// IR (cheap — function bodies are small compared to parse trees).
pub(crate) fn elide_tail_returns(body: &IrExpr) -> IrExpr {
    elide_tail_inner(body, /* in_tail = */ true)
}

fn elide_tail_inner(expr: &IrExpr, in_tail: bool) -> IrExpr {
    match expr {
        IrExpr::Return { value, .. } if in_tail => {
            // Tail-position Return: peel the wrapper.
            elide_tail_inner(value, true)
        }

        IrExpr::Block { id, stmts, span } => {
            let n = stmts.len();
            let new_stmts: Vec<IrExpr> = stmts
                .iter()
                .enumerate()
                .map(|(i, s)| elide_tail_inner(s, in_tail && i == n - 1))
                .collect();
            IrExpr::Block {
                id: *id,
                stmts: new_stmts,
                span: *span,
            }
        }

        IrExpr::Match {
            id,
            scrutinee,
            arms,
            span,
        } => {
            use ridge_ir::IrArm;
            let new_scrutinee = Box::new(elide_tail_inner(scrutinee, false));
            let new_arms = arms
                .iter()
                .map(|arm| IrArm {
                    pat: arm.pat.clone(),
                    when: arm.when.clone(),
                    body: elide_tail_inner(&arm.body, in_tail),
                    span: arm.span,
                })
                .collect();
            IrExpr::Match {
                id: *id,
                scrutinee: new_scrutinee,
                arms: new_arms,
                span: *span,
            }
        }

        IrExpr::LetIn {
            id,
            pat,
            value,
            body,
            span,
        } => {
            let new_value = Box::new(elide_tail_inner(value, false));
            let new_body = Box::new(elide_tail_inner(body, in_tail));
            IrExpr::LetIn {
                id: *id,
                pat: pat.clone(),
                value: new_value,
                body: new_body,
                span: *span,
            }
        }

        IrExpr::VarIn {
            id,
            name,
            ty,
            value,
            body,
            span,
        } => {
            let new_value = Box::new(elide_tail_inner(value, false));
            let new_body = Box::new(elide_tail_inner(body, in_tail));
            IrExpr::VarIn {
                id: *id,
                name: name.clone(),
                ty: ty.clone(),
                value: new_value,
                body: new_body,
                span: *span,
            }
        }

        // All other nodes: return a clone (no Return can be nested in non-tail
        // positions in these variants without having been caught above).
        other => other.clone(),
    }
}

// ── 5. lower_fn_body ─────────────────────────────────────────────────────────

/// Integration point for item-level fn body lowering (called by T8).
///
/// - If there are no non-tail `Return` nodes: elide tail `Return`s and lower
///   directly — no try/catch wrapper.
/// - Otherwise: lower directly (non-tail `Return` arms in `lower_expr` emit
///   throws) and wrap the result in a `try/catch` frame.
///
/// This satisfies the "tail-position `Return` does not emit a try/catch
/// wrapper".
pub(crate) fn lower_fn_body(body: &IrExpr) -> Result<CErlExpr, CodegenError> {
    let mut scope = LocalScope::new();
    if has_non_tail_return(body) {
        // Non-tail Returns exist: lower as-is (throw form) and wrap.
        let lowered = lower_expr_in_scope(body, &mut scope)?;
        Ok(wrap_with_return_catch(lowered))
    } else {
        // All Returns (if any) are in tail position: elide them and lower.
        let elided = elide_tail_returns(body);
        lower_expr_in_scope(&elided, &mut scope)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit};
    use ridge_ast::Span;
    use ridge_ir::{IrExpr, IrLit, IrNodeId};

    fn sp() -> Span {
        Span::point(0)
    }
    fn node() -> IrNodeId {
        IrNodeId(0)
    }

    fn lit_int(n: i64) -> IrExpr {
        IrExpr::Lit {
            id: node(),
            value: IrLit::Int(n),
            span: sp(),
        }
    }

    // ── lower_return shape ────────────────────────────────────────────────────

    #[test]
    fn return_lower_return_shape() {
        let result = lower_return(CErlExpr::Lit(CErlLit::Int(5)));
        match result {
            CErlExpr::Call {
                module,
                fn_name,
                args,
            } => {
                assert_eq!(module.0, "erlang");
                assert_eq!(fn_name.0, "throw");
                assert_eq!(args.len(), 1);
                match &args[0] {
                    CErlExpr::Tuple(elems) => {
                        assert_eq!(elems.len(), 2);
                        assert!(
                            matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "ridge_return")
                        );
                        assert!(matches!(elems[1], CErlExpr::Lit(CErlLit::Int(5))));
                    }
                    other => panic!("expected Tuple, got {other:?}"),
                }
            }
            other => panic!("expected Call(erlang:throw), got {other:?}"),
        }
    }

    // ── has_non_tail_return ───────────────────────────────────────────────────

    #[test]
    fn return_has_non_tail_return_false_for_no_returns() {
        // No Return nodes at all → false.
        let body = lit_int(1);
        assert!(!has_non_tail_return(&body));
    }

    #[test]
    fn return_has_non_tail_return_false_for_tail_only() {
        // Block { stmts: [Lit 1, Return Lit 5] } — Return is last → tail → false.
        let body = IrExpr::Block {
            id: node(),
            stmts: vec![
                lit_int(1),
                IrExpr::Return {
                    id: node(),
                    value: Box::new(lit_int(5)),
                    span: sp(),
                },
            ],
            span: sp(),
        };
        assert!(!has_non_tail_return(&body));
    }

    #[test]
    fn return_has_non_tail_return_true_for_non_tail() {
        // Block { stmts: [Return Lit 5, Lit 1] } — Return is NOT last → non-tail → true.
        let body = IrExpr::Block {
            id: node(),
            stmts: vec![
                IrExpr::Return {
                    id: node(),
                    value: Box::new(lit_int(5)),
                    span: sp(),
                },
                lit_int(1),
            ],
            span: sp(),
        };
        assert!(has_non_tail_return(&body));
    }

    #[test]
    fn return_does_not_recurse_into_lambdas() {
        use ridge_types::CapabilitySet;
        // Lambda body has a non-tail Return; outer fn has no Return.
        // Outer has_non_tail_return should return false (lambda is opaque).
        let lambda_body = IrExpr::Block {
            id: node(),
            stmts: vec![
                IrExpr::Return {
                    id: node(),
                    value: Box::new(lit_int(5)),
                    span: sp(),
                },
                lit_int(1),
            ],
            span: sp(),
        };
        let body = IrExpr::Lambda {
            id: node(),
            params: vec![],
            body: Box::new(lambda_body),
            caps: CapabilitySet::default(),
            span: sp(),
        };
        // Outer fn body is just the lambda — no Returns at the outer level.
        assert!(!has_non_tail_return(&body));
    }

    // ── lower_fn_body — no wrap for tail-only ─────────────────────────────────

    #[test]
    fn return_lower_fn_body_no_wrap() {
        // Body is `Return { value: Lit 5 }` — tail position → no wrap.
        let body = IrExpr::Return {
            id: node(),
            value: Box::new(lit_int(5)),
            span: sp(),
        };
        let result = lower_fn_body(&body).unwrap();
        // Should be just `Lit 5`, no Try, no Call(erlang:throw).
        assert!(matches!(result, CErlExpr::Lit(CErlLit::Int(5))));
    }

    #[test]
    fn return_lower_fn_body_with_wrap() {
        // Block { stmts: [Return Lit 5, Lit 1] } — Return is non-tail → wrap.
        let body = IrExpr::Block {
            id: node(),
            stmts: vec![
                IrExpr::Return {
                    id: node(),
                    value: Box::new(lit_int(5)),
                    span: sp(),
                },
                lit_int(1),
            ],
            span: sp(),
        };
        let result = lower_fn_body(&body).unwrap();
        // Outermost node must be Try.
        assert!(
            matches!(result, CErlExpr::Try { .. }),
            "expected Try, got {result:?}"
        );
    }
}
