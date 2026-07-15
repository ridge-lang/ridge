//! Central dispatch table for lowering `IrExpr` nodes to `CErlExpr`.
//!
//! Arms lowered here:
//! - §4.1  `IrExpr::Lit`       → [`lower_lit`] in `lit.rs`
//! - §4.2  `IrExpr::Local`     → `CErlExpr::Var` with [`name_to_erl_var`] mangling
//! - §4.3  `IrExpr::Symbol`    → [`lower_symbol`] router in `symbol.rs`
//! - §4.4  `IrExpr::Call`      → [`lower_call`]: static `IrExpr::Symbol` callee
//!   (Constructor dispatch; Prelude dispatch; Stdlib bridge map; Local/External)
//!   and dynamic callee (anything else → `CErlExpr::Apply`). The `LetIn` recursive
//!   inner-fn `letrec` path (OQ-L012) is live end-to-end with Lambda lowering.
//! - §4.5  `IrExpr::Lambda`    → [`lower_lambda`]: `CErlExpr::Fun { params, body }`.
//!   `caps` is erased (Model B capability erasure). Lambda body is lowered in a fresh per-lambda scope.
//! - §4.6  `IrExpr::LetIn`     → `CErlExpr::Let` or `CErlExpr::Case` (destructuring)
//! - §4.7  `IrExpr::VarIn`     → `CErlExpr::Let` with SSA-index-0 binding
//! - §4.8  `IrExpr::Assign`    → handled inline by the Block lowerer
//! - §4.9  `IrExpr::Return`    → throw form via `return_::lower_return`
//! - §4.10 `IrExpr::Block`     → `CErlExpr::Do` chain (right-fold)
//! - §4.11 `IrExpr::Match`     → `CErlExpr::Case`
//! - §4.12 `IrExpr::Construct` → [`lower_construct`]: `MapLit` (Record) or `Tuple`/`Atom`
//!   (UnionVariant/Prelude); OQ-CG004 `with` peephole emits `MapUpdate` instead of `MapLit`.
//! - §4.13 `IrExpr::Field`     → [`lower_field`]: `call 'maps':'get'(Atom, Base)`.
//! - §4.14 `IrExpr::ListLit`   → `CErlExpr::ListLit`
//! - §4.15 `IrExpr::Tuple`     → `CErlExpr::Tuple`
//! - §4.16 `IrExpr::Cons`      → `CErlExpr::Cons`
//!
//! All other `IrExpr` variants return a deferred `IrShapeMalformed` error.

// These helpers are exercised from test suites and from the module-level
// entry points in the codegen pipeline.
#![allow(dead_code)]
// pub(crate) on items in a pub(crate) module is redundant per clippy; we keep
// it anyway for explicitness per plan §2.2 — suppress the lint here.
#![allow(clippy::redundant_pub_crate)]
// lower_expr_in_scope is a linear dispatch table over a large enum; it will
// always be close to the 100-line limit.  Allow the natural size rather than
// splitting the match into artificial sub-functions.
#![allow(clippy::too_many_lines)]

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlLit, CErlVar};
use crate::error::CodegenError;
use crate::letrec_detect::body_references_local;
use crate::lit::lower_lit;
use crate::messaging::{lower_ask, lower_send, lower_spawn};
use crate::pat::lower_pat;
use crate::return_::lower_return;
use crate::scope::{ssa_var, LocalScope};
use crate::stdlib_map::{self, BridgeTarget};
use crate::symbol::lower_symbol;
use ridge_ir::{AssignTarget, CtorKind, IrExpr, IrLit, IrParam, IrPat, SymbolRef};

// ── Variable mangling (§4.2) ─────────────────────────────────────────────────

/// Mangle a Ridge local-variable name into a legal Core Erlang variable name.
///
/// Erlang variables must start with an uppercase letter.  Ridge identifiers
/// start lowercase (or with underscores for Phase-5-synthesised names).
///
/// **Algorithm:**
/// 1. Strip all leading underscores.
/// 2. Split the remaining string on `'_'`.
/// 3. Capitalise the first letter of each non-empty segment, leaving the rest
///    as-is (preserving any embedded uppercase from Phase-5 synthesised names).
/// 4. Join the capitalised segments (no separator).
/// 5. Prepend `"V_"`.
///
/// Examples:
/// - `"count"`       → `"V_Count"`
/// - `"__prop_ok"`   → `"V_PropOk"`
/// - `"__with_base"` → `"V_WithBase"`
///
/// SSA suffixes (`V_Count1`, `V_Count2`, …) are managed by `LocalScope`
/// — this function only implements the base mangling.
pub(crate) fn name_to_erl_var(name: &str) -> String {
    // 1. Replace `$` with `D` (dollar sign is not valid in Erlang variable
    //    names). Dictionary parameter names use `$` as a sigil (e.g.
    //    `$dict_ToText_0`); replace it so the generated variable is legal.
    let no_dollar = name.replace('$', "D");

    // 2. Strip leading underscores.
    let stripped = no_dollar.trim_start_matches('_');

    // 3. Split on '_', capitalise each non-empty segment, join.
    let capitalised: String = stripped
        .split('_')
        .filter(|seg| !seg.is_empty())
        .map(capitalise_first)
        .collect();

    // 4. Prepend the `V_` prefix.
    format!("V_{capitalised}")
}

/// Capitalise the first byte of `s`, leaving the rest unchanged.
///
/// Only handles ASCII — Ridge identifiers are ASCII (per spec §2.1).
fn capitalise_first(s: &str) -> String {
    let mut chars = s.chars();
    chars.next().map_or_else(String::new, |first| {
        let mut out = first.to_uppercase().to_string();
        out.push_str(chars.as_str());
        out
    })
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// `'true'` atom expression — used as the default guard for `case` clauses.
fn lit_true() -> CErlExpr {
    CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into())))
}

/// `'true'` literal pattern — used when an arm's guard has been lifted out
/// of the clause-guard position into the body's case-of-true dispatch.
fn lit_true_pat() -> crate::core_ast::CErlPat {
    crate::core_ast::CErlPat::Lit(CErlLit::Atom(CErlAtom("true".into())))
}

/// Return `true` if `erlang:<fn_name>/<arity>` is a BEAM guard BIF — i.e. one
/// of the functions BEAM permits in `case` clause-guard position.
///
/// The list mirrors the reference manual's "Guards" section: arithmetic and
/// bitwise operators, term-info accessors, type-check predicates, the boolean
/// connectives, and the comparison operators. Anything outside this list is
/// either not callable in a guard at all (most of `erlang:*` falls here) or
/// not exposed via the `erlang:` module name.
//
// Each arm groups guard BIFs by arity. The arms intentionally share the
// `true` body; merging them by union of patterns would erase the arity
// grouping that documents which BIFs are valid at which call shape.
#[allow(clippy::match_same_arms)]
fn is_erlang_guard_bif(fn_name: &str, arity: usize) -> bool {
    match (fn_name, arity) {
        // Arity 0.
        ("self", 0) => true,
        // Arity 1.
        (
            "abs" | "bit_size" | "bnot" | "byte_size" | "ceil" | "float" | "floor" | "hd"
            | "is_atom" | "is_binary" | "is_bitstring" | "is_boolean" | "is_float" | "is_function"
            | "is_integer" | "is_list" | "is_map" | "is_number" | "is_pid" | "is_port"
            | "is_reference" | "is_tuple" | "length" | "map_size" | "node" | "not" | "round"
            | "size" | "tl" | "trunc" | "tuple_size" | "-" | "+",
            1,
        ) => true,
        // Arity 2.
        (
            "and" | "band" | "binary_part" | "bor" | "bsl" | "bsr" | "bxor" | "div" | "element"
            | "is_function" | "is_map_key" | "is_record" | "map_get" | "max" | "min" | "or" | "rem"
            | "xor" | "+" | "-" | "*" | "/" | "<" | ">" | "=:=" | "=/=" | "==" | "/=" | "=<" | ">=",
            2,
        ) => true,
        // Arity 3.
        ("binary_part" | "is_record", 3) => true,
        _ => false,
    }
}

/// Return `true` when `expr` (a guard expression) contains a call that BEAM
/// rejects in clause-guard position.
///
/// BEAM `case` clause guards only admit a small, fixed set of `erlang:` BIFs
/// — arithmetic operators, comparison operators, term-info accessors, type
/// guards, and boolean connectives. Anything else (a stdlib helper, a
/// user-defined function, or an `erlang:*` function that exists but is not a
/// guard BIF such as `erlang:list_to_binary/1`) makes the surrounding guard
/// illegal, so the arm must be rewritten out of clause-guard position via
/// [`lift_guarded_match`].
fn contains_non_bif_call(expr: &CErlExpr) -> bool {
    match expr {
        CErlExpr::Lit(_) | CErlExpr::Var(_) | CErlExpr::LocalFnRef { .. } => false,

        CErlExpr::Call {
            module: CErlAtom(m),
            fn_name: CErlAtom(f),
            args,
        } => {
            if m != "erlang" || !is_erlang_guard_bif(f, args.len()) {
                return true;
            }
            args.iter().any(contains_non_bif_call)
        }

        // `apply` indirects through a fun reference; never legal in a guard.
        CErlExpr::Apply { .. } => true,

        CErlExpr::Fun { body, .. } => contains_non_bif_call(body),
        CErlExpr::Let { value, body, .. } => {
            contains_non_bif_call(value) || contains_non_bif_call(body)
        }
        CErlExpr::LetRec { defs, body } => {
            defs.iter().any(|(_, _, e)| contains_non_bif_call(e)) || contains_non_bif_call(body)
        }
        CErlExpr::Case {
            scrutinee, clauses, ..
        } => {
            contains_non_bif_call(scrutinee)
                || clauses
                    .iter()
                    .any(|c| contains_non_bif_call(&c.guard) || contains_non_bif_call(&c.body))
        }
        CErlExpr::Do { first, then } => contains_non_bif_call(first) || contains_non_bif_call(then),
        CErlExpr::Tuple(elems) | CErlExpr::ListLit(elems) => {
            elems.iter().any(contains_non_bif_call)
        }
        CErlExpr::Cons { head, tail } => contains_non_bif_call(head) || contains_non_bif_call(tail),
        CErlExpr::MapLit(kvs) => kvs
            .iter()
            .any(|(k, v)| contains_non_bif_call(k) || contains_non_bif_call(v)),
        CErlExpr::MapUpdate { base, updates } => {
            contains_non_bif_call(base)
                || updates
                    .iter()
                    .any(|(k, v)| contains_non_bif_call(k) || contains_non_bif_call(v))
        }
        CErlExpr::Receive { clauses, after } => {
            clauses
                .iter()
                .any(|c| contains_non_bif_call(&c.guard) || contains_non_bif_call(&c.body))
                || after
                    .as_ref()
                    .is_some_and(|(t, b)| contains_non_bif_call(t) || contains_non_bif_call(b))
        }
        CErlExpr::Try { body, of, catch } => {
            contains_non_bif_call(body)
                || of
                    .iter()
                    .any(|c| contains_non_bif_call(&c.guard) || contains_non_bif_call(&c.body))
                || catch
                    .iter()
                    .any(|c| contains_non_bif_call(&c.guard) || contains_non_bif_call(&c.body))
        }
    }
}

/// One pre-lowered match arm carrying the pieces `lift_guarded_match` needs.
///
/// `guard` is `'true'` when the source arm had no `when` clause. `guard_is_safe`
/// records whether [`contains_non_bif_call`] cleared the guard for clause-guard
/// position — independent of whether the guard is literally `true`, since some
/// `when`-less arms naturally produce `'true'` as the guard already.
struct LoweredArm {
    pattern: crate::core_ast::CErlPat,
    guard: CErlExpr,
    body: CErlExpr,
    guard_is_safe: bool,
}

/// Rewrite a match whose arm guards include calls outside `erlang:*` into a
/// chain of nested `case` expressions.
///
/// The transformation is needed because Core Erlang clause guards admit only
/// calls to guard BIFs. Ridge guards routinely contain calls to stdlib helpers
/// (`std.int:mod/2` when the user writes `n % k`, `std.op:eq/2` and friends
/// for any non-trivial equality) which `erlc` rejects with
/// `illegal guard expression`.
///
/// Each arm whose guard is not BIF-safe is rewritten from
///
/// ```text
/// Pat when Guard -> Body
/// ```
///
/// to
///
/// ```text
/// Pat -> case Guard of 'true' -> Body ; _ -> Rest end
/// ```
///
/// followed by a wildcard catch-all `_ -> Rest` that handles scrutinees which
/// did not match `Pat`. `Rest` is the recursive transformation of the
/// remaining arms against the same (already-bound) scrutinee variable.
/// Arms that come *after* a lifted arm are folded entirely into `Rest`, not
/// re-emitted as siblings of the outer case.
///
/// Arms whose guard is already BIF-safe (or carries no guard at all) are
/// emitted as ordinary clauses with their original `when`.
///
/// `scrut_var` is the variable the scrutinee has been bound to — the caller
/// wraps the result of this function in a `let` that introduces it, so the
/// scrutinee is evaluated exactly once even though the variable is referenced
/// at every nesting level.
fn lift_guarded_match(scrut_var: &CErlVar, arms: &[LoweredArm]) -> CErlExpr {
    lift_guarded_match_at_depth(scrut_var, arms, 0)
}

/// Internal worker for [`lift_guarded_match`] that threads a `depth` counter so
/// each lifted level gets a uniquely-named `V_LiftedRest<depth>` continuation
/// thunk. Lexical scoping would let the names collide harmlessly, but giving
/// nested levels distinct names keeps the generated Core Erlang readable.
fn lift_guarded_match_at_depth(scrut_var: &CErlVar, arms: &[LoweredArm], depth: u32) -> CErlExpr {
    let mut clauses: Vec<CErlClause> = Vec::with_capacity(arms.len() + 1);

    for (i, arm) in arms.iter().enumerate() {
        if arm.guard_is_safe {
            clauses.push(CErlClause {
                pattern: arm.pattern.clone(),
                guard: arm.guard.clone(),
                body: arm.body.clone(),
            });
            continue;
        }

        // Non-BIF-safe guard — lift it into the clause body. The guard-case's
        // fall-through arm and the outer wildcard catch-all BOTH need the same
        // remaining-arms expression. A naïve `rest.clone()` in each slot fans
        // out exponentially over a chain of lifted arms (N×K explosion for K
        // unsafe arms with N tags downstream). Hoist `rest` into a 0-arg fun
        // bound by an outer `let`, then reference it from both clause bodies
        // via `apply`. The fun's body is evaluated at most once per dispatch.
        let rest_expr = if i + 1 < arms.len() {
            lift_guarded_match_at_depth(scrut_var, &arms[i + 1..], depth + 1)
        } else {
            // No remaining arms; fall-through replicates the runtime
            // behaviour of an unmatched `case` clause: a `case_clause`
            // exception.
            CErlExpr::Call {
                module: CErlAtom("erlang".into()),
                fn_name: CErlAtom("error".into()),
                args: vec![CErlExpr::Lit(CErlLit::Atom(CErlAtom("case_clause".into())))],
            }
        };

        let rest_var = CErlVar(format!("V_LiftedRest{depth}"));
        let invoke_rest = || CErlExpr::Apply {
            callee: Box::new(CErlExpr::Var(rest_var.clone())),
            args: vec![],
        };

        let body_with_guard = CErlExpr::Case {
            scrutinee: Box::new(arm.guard.clone()),
            clauses: vec![
                CErlClause {
                    pattern: lit_true_pat(),
                    guard: lit_true(),
                    body: arm.body.clone(),
                },
                CErlClause {
                    pattern: crate::core_ast::CErlPat::Wild,
                    guard: lit_true(),
                    body: invoke_rest(),
                },
            ],
        };

        clauses.push(CErlClause {
            pattern: arm.pattern.clone(),
            guard: lit_true(),
            body: body_with_guard,
        });
        clauses.push(CErlClause {
            pattern: crate::core_ast::CErlPat::Wild,
            guard: lit_true(),
            body: invoke_rest(),
        });

        let case_expr = CErlExpr::Case {
            scrutinee: Box::new(CErlExpr::Var(scrut_var.clone())),
            clauses,
        };

        return CErlExpr::Let {
            var: rest_var,
            value: Box::new(CErlExpr::Fun {
                params: vec![],
                body: Box::new(rest_expr),
            }),
            body: Box::new(case_expr),
        };
    }

    // Every arm was BIF-safe — emit the ordinary case shape (the call site's
    // fast path already covers this; keep the branch as defensive coverage in
    // case `lift_guarded_match` is called on an all-safe slice via recursion).
    CErlExpr::Case {
        scrutinee: Box::new(CErlExpr::Var(scrut_var.clone())),
        clauses,
    }
}

// ── Central dispatch ─────────────────────────────────────────────────────────

/// Lower an [`IrExpr`] to a [`CErlExpr`] with a fresh empty [`LocalScope`].
///
/// This is the outer entry point.  Callers that already carry a scope (e.g.
/// the Block lowerer, Match arm lowerer) should call [`lower_expr_in_scope`]
/// directly.
pub(crate) fn lower_expr(expr: &IrExpr) -> Result<CErlExpr, CodegenError> {
    let mut scope = LocalScope::new();
    lower_expr_in_scope(expr, &mut scope)
}

/// Lower an [`IrExpr`] to a [`CErlExpr`], threading the given [`LocalScope`].
///
/// The scope tracks `var`-bound locals (§3.12).  Every SSA-index lookup and
/// bump goes through `scope`.
pub(crate) fn lower_expr_in_scope(
    expr: &IrExpr,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    match expr {
        // §4.1 — Literal values.
        IrExpr::Lit { value, span, .. } => lower_lit(value, *span).map(CErlExpr::Lit),

        // §4.2 — Local variable reference.
        // A bare local resolves to a `LocalFnRef` ONLY when it names an active
        // inner recursive fn — a `letrec` binding registered for the duration of
        // its own lowering (see the `LetRec` path below and the actor variants in
        // `init.rs`, which add the name to `letrec_locals`). Gating on
        // `letrec_locals` rather than `fn_arity` alone is load-bearing: `fn_arity`
        // is also seeded with every top-level module fn, so a parameter / `let` /
        // `var` that happens to share a name with a top-level fn would otherwise be
        // miscompiled into a reference to that fn (a curried `#Fun<...>`) instead of
        // the bound variable. Otherwise, if the name is a `var`-bound local resolve
        // to its current SSA-suffixed variable; else use the bare mangled name.
        IrExpr::Local { name, span, .. } => {
            if scope.letrec_locals.contains(name.as_str()) {
                if let Some(&arity) = scope.fn_arity.get(name.as_str()) {
                    return Ok(CErlExpr::LocalFnRef {
                        name: CErlAtom(name.clone()),
                        arity,
                    });
                }
            }
            let _ = span;
            // `__state` is the synthetic base local emitted by `lower_ident` for
            // every state-field read inside an actor handler or init body. It
            // must always resolve to the latest V_State<n> known to the scope
            // (bumped by the actor-body lowering on each `<-` assign), not via
            // the generic `var`-SSA table. Without this, a read of a state field
            // that follows an assign in the same handler invocation would see
            // the pre-assign V_State value.
            if name == "__state" {
                return Ok(crate::init::state_expr(scope.actor_state_idx));
            }
            let mangled = name_to_erl_var(name);
            let var = match scope.current_index(name) {
                Some(idx) => ssa_var(&mangled, idx),
                None => CErlVar(mangled),
            };
            Ok(CErlExpr::Var(var))
        }

        // §4.3 — Symbol reference (top-level fn, stdlib, constructor, …).
        // Pass the fn-arity table from the scope so that SymbolRef::Local used
        // as a value can be resolved to a LocalFnRef.
        // B-6: also pass actor_parent so that parent-module symbol refs used as
        // values emit qualified calls instead of LocalFnRef (which would be
        // undefined in the actor's separate BEAM module).
        IrExpr::Symbol { sym, span, .. } => {
            let actor_parent = scope
                .actor_parent
                .as_ref()
                .map(|(id, beam)| (*id, beam.as_ref()));
            lower_symbol(sym, *span, &scope.fn_arity, actor_parent)
        }

        // §4.14 — List literal.
        IrExpr::ListLit { elems, .. } => elems
            .iter()
            .map(|e| lower_expr_in_scope(e, scope))
            .collect::<Result<Vec<_>, _>>()
            .map(CErlExpr::ListLit),

        // §4.15 — Tuple literal.
        IrExpr::Tuple { elems, .. } => elems
            .iter()
            .map(|e| lower_expr_in_scope(e, scope))
            .collect::<Result<Vec<_>, _>>()
            .map(CErlExpr::Tuple),

        // §4.16 — Cons cell (`x :: xs`).
        IrExpr::Cons { head, tail, .. } => {
            let head_lowered = lower_expr_in_scope(head, scope)?;
            let tail_lowered = lower_expr_in_scope(tail, scope)?;
            Ok(CErlExpr::Cons {
                head: Box::new(head_lowered),
                tail: Box::new(tail_lowered),
            })
        }

        // ── LetIn, VarIn, Assign, Return, Block, Match ───────────────────────

        // §4.6 — LetIn: simple Bind(name, None) → Let; other patterns → Case.
        // OQ-L012 (Phase 5): recursive inner-fn detection: if value is Lambda
        // and the lambda body references the bound name, we emit LetRec.
        // Lambda lowering completes the rec-inner-fn path.
        IrExpr::LetIn {
            pat, value, body, ..
        } => {
            if let IrPat::Bind {
                name, inner: None, ..
            } = pat
            {
                // Simple case: single-name bind, no as-pattern.
                // Check for recursive inner-fn (OQ-L012) — if value is a
                // Lambda and the lambda body references `name`, we should
                // emit LetRec rather than Let.
                if let IrExpr::Lambda {
                    params: lambda_params,
                    body: lambda_body,
                    ..
                } = value.as_ref()
                {
                    if body_references_local(lambda_body, name) {
                        // LetRec emission path for recursive inner functions.
                        // 1. Determine arity from the lambda params.
                        #[allow(clippy::cast_possible_truncation)]
                        let arity = lambda_params.len() as u32;
                        // 2. Register the inner fn in fn_arity (for arity) and
                        //    letrec_locals (the active-local-fn marker the
                        //    `IrExpr::Local` arm gates on) so that references to it
                        //    inside the lambda body and in the letrec body emit
                        //    LocalFnRef (not Var). Both are Arc-shared; make_mut
                        //    clones if needed.
                        std::sync::Arc::make_mut(&mut scope.fn_arity).insert(name.clone(), arity);
                        std::sync::Arc::make_mut(&mut scope.letrec_locals).insert(name.clone());
                        let lowered_lambda = lower_expr_in_scope(value, scope)?;
                        let lowered_body = lower_expr_in_scope(body, scope)?;
                        // Remove from both to avoid leaking into the outer scope.
                        std::sync::Arc::make_mut(&mut scope.fn_arity).remove(name.as_str());
                        std::sync::Arc::make_mut(&mut scope.letrec_locals).remove(name.as_str());
                        return Ok(CErlExpr::LetRec {
                            defs: vec![(CErlAtom(name.clone()), arity, lowered_lambda)],
                            body: Box::new(lowered_body),
                        });
                    }
                }
                // Plain Let.
                let lowered_value = lower_expr_in_scope(value, scope)?;
                let lowered_body = lower_expr_in_scope(body, scope)?;
                Ok(CErlExpr::Let {
                    var: CErlVar(name_to_erl_var(name)),
                    value: Box::new(lowered_value),
                    body: Box::new(lowered_body),
                })
            } else {
                // Destructuring or as-pattern: emit a Case with one clause.
                let lowered_value = lower_expr_in_scope(value, scope)?;
                let lowered_body = lower_expr_in_scope(body, scope)?;
                Ok(CErlExpr::Case {
                    scrutinee: Box::new(lowered_value),
                    clauses: vec![CErlClause {
                        pattern: lower_pat(pat)?,
                        guard: lit_true(),
                        body: lowered_body,
                    }],
                })
            }
        }

        // §4.7 — VarIn: introduce the var-bound local at SSA index 0.
        IrExpr::VarIn {
            name, value, body, ..
        } => {
            let mangled = name_to_erl_var(name);
            let idx = scope.bump(name); // always returns 0 for a fresh name
            let lowered_value = lower_expr_in_scope(value, scope)?;
            let lowered_body = lower_expr_in_scope(body, scope)?;
            Ok(CErlExpr::Let {
                var: ssa_var(&mangled, idx),
                value: Box::new(lowered_value),
                body: Box::new(lowered_body),
            })
        }

        // §4.8 — Assign: only valid inside a Block (handled by lower_block_stmts).
        // A standalone Assign (no enclosing Block) is a Phase-5 invariant
        // violation.  The StateField subcase is deferred to T9.
        IrExpr::Assign { target, span, .. } => match target {
            AssignTarget::Local { .. } => Err(CodegenError::IrShapeMalformed {
                variant: "IrExpr::Assign",
                span: *span,
                detail: "Assign without enclosing Block — Phase 5 invariant violated".into(),
            }),
            AssignTarget::StateField { name, .. } => Err(CodegenError::IrShapeMalformed {
                variant: "IrExpr::Assign",
                span: *span,
                detail: state_field_assign_detail(name, scope),
            }),
        },

        // §4.9 — Return: emit the throw form at expression scope.
        // Tail-position elision and try/catch wrapping happen at fn-body level
        // via return_::lower_fn_body.
        IrExpr::Return { value, .. } => {
            let lowered_value = lower_expr_in_scope(value, scope)?;
            Ok(lower_return(lowered_value))
        }

        // §4.10 — Block: sequence via Do, with Assign-as-Let promotion.
        IrExpr::Block { stmts, span, .. } => lower_block_stmts(stmts, scope, *span),

        // §4.11 — Match: emit Case; per-arm scope clone prevents cross-arm leakage.
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            let lowered_scrutinee = lower_expr_in_scope(scrutinee, scope)?;
            // Lower each arm's pattern, guard, body once; flag whether the
            // guard contains a call that BEAM rejects in clause-guard position.
            let lowered_arms: Vec<LoweredArm> = arms
                .iter()
                .map(|arm| {
                    let mut arm_scope = scope.clone();
                    let guard = match &arm.when {
                        Some(w) => lower_expr_in_scope(w, &mut arm_scope)?,
                        None => lit_true(),
                    };
                    let guard_is_safe = !contains_non_bif_call(&guard);
                    Ok(LoweredArm {
                        pattern: lower_pat(&arm.pat)?,
                        guard,
                        body: lower_expr_in_scope(&arm.body, &mut arm_scope)?,
                        guard_is_safe,
                    })
                })
                .collect::<Result<Vec<_>, CodegenError>>()?;

            // Fast path: every guard fits BEAM's whitelist — emit the
            // ordinary `case Scrut of P when G -> B; ... end` shape.
            if lowered_arms.iter().all(|a| a.guard_is_safe) {
                let clauses = lowered_arms
                    .into_iter()
                    .map(|a| CErlClause {
                        pattern: a.pattern,
                        guard: a.guard,
                        body: a.body,
                    })
                    .collect();
                return Ok(CErlExpr::Case {
                    scrutinee: Box::new(lowered_scrutinee),
                    clauses,
                });
            }

            // Lift path: at least one guard calls a non-BIF function. Bind
            // the scrutinee to a fresh local so the nested cases produced by
            // `lift_guarded_match` reference it once each without
            // re-evaluating the source expression.
            let scrut_idx = scope.bump("_match_scrut");
            let scrut_var = ssa_var("V_MatchScrut", scrut_idx);
            let lifted = lift_guarded_match(&scrut_var, &lowered_arms);
            Ok(CErlExpr::Let {
                var: scrut_var,
                value: Box::new(lowered_scrutinee),
                body: Box::new(lifted),
            })
        }

        // ── Lambda, Call, Construct, Field, Send, Ask, Spawn ─────────────────

        // §4.5 — Lambda: fun (P1, ..., PN) -> Body end.
        // `caps` is erased (Model B capability erasure).
        IrExpr::Lambda { params, body, .. } => lower_lambda(params, body, scope),

        // §4.4 — Call: static callee → dispatch by SymbolRef; dynamic → Apply.
        IrExpr::Call {
            callee, args, span, ..
        } => lower_call(callee, args, *span, scope),
        // §4.12 — Constructor: Record → MapLit; UnionVariant/Prelude → Tuple or
        // bare Atom.
        IrExpr::Construct {
            ctor, fields, span, ..
        } => lower_construct(ctor, fields, *span, scope),

        // §4.5 — `with` update.
        //
        // When the base is statically a map term the BEAM type analyser can
        // see — a record literal or a nested `with` rooted in one — emit the
        // native map-update `~{ k => v | Base }~`, which compiles to the
        // inline `put_map_assoc`.
        //
        // When the base type is opaque to the analyser — a function parameter,
        // a field read, any call result — that instruction fails the +5
        // validator's consistency check (`{needed,{t_map,_,_}},{actual,any}`).
        // Route those through `maps:merge/2`, an ordinary BIF call with no
        // static map-type requirement and the same update semantics: the
        // second map's values win, every untouched key is preserved.
        IrExpr::RecordUpdate { base, updates, .. } => {
            let base_expr = lower_expr_in_scope(base, scope)?;
            let kvs = updates
                .iter()
                .map(|(key, value)| {
                    let k = CErlExpr::Lit(CErlLit::Atom(CErlAtom(key.clone())));
                    let v = lower_expr_in_scope(value, scope)?;
                    Ok((k, v))
                })
                .collect::<Result<Vec<_>, CodegenError>>()?;
            if cerl_is_static_map(&base_expr) {
                Ok(CErlExpr::MapUpdate {
                    base: Box::new(base_expr),
                    updates: kvs,
                })
            } else {
                Ok(CErlExpr::Call {
                    module: CErlAtom("maps".into()),
                    fn_name: CErlAtom("merge".into()),
                    args: vec![base_expr, CErlExpr::MapLit(kvs)],
                })
            }
        }

        // §4.13 — Field projection: emit `call 'maps':'get'(Atom key, Base)`.
        IrExpr::Field {
            base, field, span, ..
        } => lower_field(base, field, *span, scope),
        // §4.17 — Send (`!`): route through ridge_rt:send_op/2 (OQ-E004).
        IrExpr::Send {
            handle,
            message,
            args,
            span,
            ..
        } => lower_send(handle, message, args, *span, scope),

        // §4.18 — Ask (`?>`): route through ridge_rt:ask/3, 5000 ms default (OQ-E001, OQ-E004).
        IrExpr::Ask {
            handle,
            message,
            args,
            timeout,
            span,
            ..
        } => lower_ask(handle, message, args, timeout.as_ref(), *span, scope),

        // §4.19 — Spawn: route through ridge_rt:spawn_actor/3 (OQ-E006).
        IrExpr::Spawn {
            actor, args, span, ..
        } => lower_spawn(actor, args, *span, scope),

        // IrExpr is #[non_exhaustive]; catch future variants defensively.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr",
            span: ridge_ast::Span::point(0),
            detail: "unrecognised IrExpr variant — no lowering arm defined".into(),
        }),
    }
}

/// Is `e` a Core Erlang term the BEAM type analyser already sees as a map?
///
/// Only map literals and native map updates qualify: both lower to a map term
/// whose type is statically known, so a `put_map_assoc` over them passes the
/// +5 validator. A `Var`, a `Call`, or any other expression has type `any` as
/// far as the analyser is concerned, and a `put_map_assoc` over `any` trips
/// `{bad_type,{needed,{t_map,any,any}},{actual,any}}`. Those base expressions
/// take the `maps:merge/2` path instead. A `MapUpdate` is only ever emitted
/// over a base that already satisfied this test, so it is itself always a map.
const fn cerl_is_static_map(e: &CErlExpr) -> bool {
    matches!(e, CErlExpr::MapLit(_) | CErlExpr::MapUpdate { .. })
}

// ── Block helper (§4.10) ──────────────────────────────────────────────────────

/// Build the detail string for an `IrExpr::Assign { AssignTarget::StateField }`
/// that reaches the regular expr-lowering path (`lower_expr_in_scope` /
/// `lower_block_stmts`) instead of the actor-handler path
/// (`lower_actor_block_w` / `lower_expr_in_actor_context_w`).
///
/// Two cases are distinguished by `scope.actor_parent`:
///
/// 1. `actor_parent.is_some()` — we ARE inside an actor handler, but the
///    assign sits in a nested `fn` (lambda) body that the actor-context
///    walk never reaches.  Lambdas don't have access to the implicit
///    `gen_server` state in 0.2.x, so this won't lower as-is.  Emit the
///    actionable hint pointing at the canonical workaround.
/// 2. `actor_parent.is_none()` — the assign appears in a top-level `fn`
///    with no actor parent at all.  That's a genuine "wrong shape" case
///    (typecheck should have caught it earlier); keep the legacy phrasing
///    so the existing test suite stays satisfied.
fn state_field_assign_detail(name: &str, scope: &LocalScope) -> String {
    if scope.actor_parent.is_some() {
        format!(
            "state field `{name}` cannot be assigned from inside a nested `fn` (lambda).  \
             State assigns are only valid at the immediate handler scope; the implicit \
             gen_server state is not in scope inside an inner-fn body.  Workaround: \
             extract the loop to a top-level helper that takes the running totals as \
             parameters and returns them as a record, then assign once in the handler \
             body from the returned record's fields.  See dx-tests/producer-consumer \
             for an example."
        )
    } else {
        "StateField Assign requires actor-handler context".to_string()
    }
}

/// Lower a `Block`'s statement slice to a right-folded `Do` chain (§4.10).
///
/// Assign statements are promoted to `Let` bindings (the continuation is the
/// rest of the block).  Phase 5 invariants:
///
/// - `stmts` must be non-empty.
/// - `LetIn`/`VarIn` must not appear as Block stmts (they are continuation-form).
fn lower_block_stmts(
    stmts: &[IrExpr],
    scope: &mut LocalScope,
    span: ridge_ast::Span,
) -> Result<CErlExpr, CodegenError> {
    match stmts {
        [] => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Block",
            span,
            detail: "Block with zero stmts — Phase 5 invariant violated".into(),
        }),
        [single] => {
            // Single statement: its value is the block's value.
            lower_expr_in_scope(single, scope)
        }
        [first, rest @ ..] => {
            match first {
                // Assign(Local) → let-bind with the rest as the continuation.
                IrExpr::Assign {
                    target: AssignTarget::Local { name, .. },
                    value,
                    ..
                } => {
                    let mangled = name_to_erl_var(name);
                    // Lower the RHS against the pre-assignment scope. `x <- x + 1`
                    // must read the current SSA version of `x`, not the one the
                    // binder is about to introduce, so the value is lowered before
                    // the bump; only the binder advances the index.
                    let lowered_value = lower_expr_in_scope(value, scope)?;
                    let new_idx = scope.bump(name);
                    let lowered_rest = lower_block_stmts(rest, scope, span)?;
                    Ok(CErlExpr::Let {
                        var: ssa_var(&mangled, new_idx),
                        value: Box::new(lowered_value),
                        body: Box::new(lowered_rest),
                    })
                }

                // Assign(StateField) requires actor-handler context.
                IrExpr::Assign {
                    target: AssignTarget::StateField { name, .. },
                    span: assign_span,
                    ..
                } => Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Assign",
                    span: *assign_span,
                    detail: state_field_assign_detail(name, scope),
                }),

                // LetIn/VarIn as a Block stmt: Phase 5 invariant violation.
                IrExpr::LetIn { span: let_span, .. } | IrExpr::VarIn { span: let_span, .. } => {
                    Err(CodegenError::IrShapeMalformed {
                        variant: "IrExpr::Block",
                        span: *let_span,
                        detail: "Phase 5 invariant violated: LetIn/VarIn must be \
                                 continuation-form, not Block stmts"
                            .into(),
                    })
                }

                // General non-binding statement: emit Do { first, then: rest }.
                other => {
                    let lowered_first = lower_expr_in_scope(other, scope)?;
                    let lowered_rest = lower_block_stmts(rest, scope, span)?;
                    Ok(CErlExpr::Do {
                        first: Box::new(lowered_first),
                        then: Box::new(lowered_rest),
                    })
                }
            }
        }
    }
}

// ── §4.12 Construct lowering ─────────────────────────────────────────────────

/// Lower `IrExpr::Construct` to a `CErlExpr` (§4.12).
///
/// Dispatch by `ctor`:
/// - `Constructor { ctor_kind: Record }` → `MapLit` (or `MapUpdate` via the
///   OQ-CG004 `with` peephole when the field slice encodes a `with` update).
/// - `Constructor { ctor_kind: UnionVariant }` → `Tuple([Atom name, v1, …])` if
///   fields are non-empty; `Lit(Atom name)` if fields are empty.
/// - `Prelude { name: "Some" | "Ok" | "Err" }` → `Tuple([Atom tag, v0])`.
/// - `Prelude { name: "None" }` → `Lit(Atom "none")`.
/// - All other `SymbolRef` variants inside `ctor` → defensive `IrShapeMalformed`.
fn lower_construct(
    ctor: &SymbolRef,
    fields: &[(String, IrExpr)],
    span: ridge_ast::Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    match ctor {
        // ── Record constructor → MapLit or MapUpdate (with peephole). ─────────
        SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            ..
        } => {
            // Record construction → full map literal. (`with` updates lower to
            // `IrExpr::RecordUpdate` → `MapUpdate`, handled in `lower_expr`.)
            let pairs = fields
                .iter()
                .map(|(key, value)| {
                    let k = CErlExpr::Lit(CErlLit::Atom(CErlAtom(key.clone())));
                    let v = lower_expr_in_scope(value, scope)?;
                    Ok((k, v))
                })
                .collect::<Result<Vec<_>, CodegenError>>()?;
            Ok(CErlExpr::MapLit(pairs))
        }

        // ── UnionVariant constructor → tagged tuple or bare atom. ─────────────
        SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            name,
            ..
        } => {
            if fields.is_empty() {
                // Zero-payload variant → bare atom `'Name'`.
                Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))))
            } else {
                // Positional tuple `{Name, v1, v2, …}` — field names dropped.
                let mut elems = Vec::with_capacity(fields.len() + 1);
                elems.push(CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))));
                for (_, value) in fields {
                    elems.push(lower_expr_in_scope(value, scope)?);
                }
                Ok(CErlExpr::Tuple(elems))
            }
        }

        // ── Prelude constructors. ─────────────────────────────────────────────
        SymbolRef::Prelude { name } if json_ctor_tag(name).is_some() => {
            // JsonValue variants → the lowercase-snake BEAM atoms that
            // `ridge_rt:json_encode/1` walks (`json_null`, `{json_int, N}`, …).
            let (tag, has_payload) = json_ctor_tag(name).unwrap_or(("", false));
            if has_payload {
                if fields.len() != 1 {
                    return Err(CodegenError::IrShapeMalformed {
                        variant: "SymbolRef::Prelude",
                        span,
                        detail: format!("Prelude '{name}' expects exactly 1 field, got {}", fields.len()),
                    });
                }
                let inner = lower_expr_in_scope(&fields[0].1, scope)?;
                Ok(CErlExpr::Tuple(vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom(tag.into()))),
                    inner,
                ]))
            } else {
                if !fields.is_empty() {
                    return Err(CodegenError::IrShapeMalformed {
                        variant: "SymbolRef::Prelude",
                        span,
                        detail: format!("Prelude '{name}' expects exactly 0 fields, got {}", fields.len()),
                    });
                }
                Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(tag.into()))))
            }
        }
        SymbolRef::Prelude { name } => match name.as_str() {
            "None" => {
                if !fields.is_empty() {
                    return Err(CodegenError::IrShapeMalformed {
                        variant: "SymbolRef::Prelude",
                        span,
                        detail: format!(
                            "Prelude 'None' expects exactly 0 fields, got {}",
                            fields.len()
                        ),
                    });
                }
                Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom("none".into()))))
            }
            "Some" => {
                if fields.len() != 1 {
                    return Err(CodegenError::IrShapeMalformed {
                        variant: "SymbolRef::Prelude",
                        span,
                        detail: format!(
                            "Prelude 'Some' expects exactly 1 field, got {}",
                            fields.len()
                        ),
                    });
                }
                let inner = lower_expr_in_scope(&fields[0].1, scope)?;
                Ok(CErlExpr::Tuple(vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("some".into()))),
                    inner,
                ]))
            }
            "Ok" => {
                if fields.len() != 1 {
                    return Err(CodegenError::IrShapeMalformed {
                        variant: "SymbolRef::Prelude",
                        span,
                        detail: format!(
                            "Prelude 'Ok' expects exactly 1 field, got {}",
                            fields.len()
                        ),
                    });
                }
                let inner = lower_expr_in_scope(&fields[0].1, scope)?;
                Ok(CErlExpr::Tuple(vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
                    inner,
                ]))
            }
            "Err" => {
                if fields.len() != 1 {
                    return Err(CodegenError::IrShapeMalformed {
                        variant: "SymbolRef::Prelude",
                        span,
                        detail: format!(
                            "Prelude 'Err' expects exactly 1 field, got {}",
                            fields.len()
                        ),
                    });
                }
                let inner = lower_expr_in_scope(&fields[0].1, scope)?;
                Ok(CErlExpr::Tuple(vec![
                    CErlExpr::Lit(CErlLit::Atom(CErlAtom("error".into()))),
                    inner,
                ]))
            }
            other => Err(CodegenError::IrShapeMalformed {
                variant: "SymbolRef::Prelude",
                span,
                detail: format!(
                    "Prelude '{other}' is not a valid Construct ctor — Phase 5 invariant violated"
                ),
            }),
        },

        // ── All other SymbolRef variants inside Construct.ctor are Phase-5
        // invariant violations: constructors must be Constructor or Prelude. ───
        SymbolRef::Local { .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Construct",
            span,
            detail: "ctor is SymbolRef::Local — not a valid constructor (Phase 5 invariant violated)".into(),
        }),
        SymbolRef::Stdlib { .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Construct",
            span,
            detail: "ctor is SymbolRef::Stdlib — not a valid constructor (Phase 5 invariant violated)".into(),
        }),
        SymbolRef::External { .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Construct",
            span,
            detail: "ctor is SymbolRef::External — not a valid constructor (Phase 5 invariant violated)".into(),
        }),
        SymbolRef::Handler { .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Construct",
            span,
            detail: "ctor is SymbolRef::Handler — not a valid constructor (Phase 5 invariant violated)".into(),
        }),
        SymbolRef::ActorType { .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Construct",
            span,
            detail: "ctor is SymbolRef::ActorType — not a valid constructor (Phase 5 invariant violated)".into(),
        }),
        // SymbolRef is #[non_exhaustive]; catch future variants.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Construct",
            span,
            detail: "ctor is an unrecognised SymbolRef variant — Phase 5 invariant violated".into(),
        }),
    }
}

// ── §4.13 Field lowering ──────────────────────────────────────────────────────

/// Lower `IrExpr::Field` to `call 'maps':'get'(Atom key, base)` (§4.13),
/// with a static-resolution peephole for typeclass dictionaries.
///
/// # Static peephole (dictionary lowering)
///
/// When `base` is a literal `IrExpr::Construct { ctor: Record, fields }` (an
/// instance dictionary) and `fields` contains a pair whose key matches `field`,
/// the lookup is folded at codegen time: instead of emitting
/// `call 'maps':'get'(Key, #{...})` the matched value is emitted directly.
///
/// This fires for every call site where the dict arg is a literal `MapLit`
/// (i.e. `DictPlan::Static` was resolved). Under coherence, essentially every
/// monomorphic call site is static, so dispatch overhead is near-zero.
///
/// The peephole only applies to syntactically-literal `Construct` nodes — it
/// does NOT apply to variables or `Local` references that happen to hold a map.
///
/// Argument order follows Erlang's `maps:get/2` convention: `(Key, Map)`.
fn lower_field(
    base: &IrExpr,
    field: &str,
    span: ridge_ast::Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    let _ = span; // span carried in error variants only; no use here

    // Static-dict peephole: fold `maps:get(K, #{K => V, ...})` → `V`.
    if let IrExpr::Construct {
        ctor:
            ridge_ir::SymbolRef::Constructor {
                ctor_kind: ridge_ir::CtorKind::Record,
                ..
            },
        fields,
        ..
    } = base
    {
        if let Some((_key, value)) = fields.iter().find(|(k, _)| k == field) {
            return lower_expr_in_scope(value, scope);
        }
    }

    let key = CErlExpr::Lit(CErlLit::Atom(CErlAtom(field.into())));
    let base_expr = lower_expr_in_scope(base, scope)?;
    Ok(CErlExpr::Call {
        module: CErlAtom("maps".into()),
        fn_name: CErlAtom("get".into()),
        args: vec![key, base_expr],
    })
}

// ── §4.5 Lambda lowering ─────────────────────────────────────────────────────

/// Lower `IrExpr::Lambda` to `CErlExpr::Fun { params, body }` (§4.5).
///
/// Each parameter is mangled via [`name_to_erl_var`] and wrapped in a
/// [`CErlVar`].  The body is lowered in a **fresh per-lambda scope** (Erlang
/// funs introduce their own variable scope; outer SSA indices must not leak
/// in).  `caps` is erased (Model B capability erasure).
fn lower_lambda(
    params: &[IrParam],
    body: &IrExpr,
    outer_scope: &LocalScope,
) -> Result<CErlExpr, CodegenError> {
    // Fresh scope — lambda body is in a new variable scope.  Outer var-bound
    // SSA indices do not flow into the lambda body (Erlang funs are closures
    // over names, not over SSA slots).
    // The fn-arity table IS inherited so that SymbolRef::Local values inside
    // lambda bodies (e.g. `List.map f items` where `f` is a local fn) resolve.
    // own_module_beam_name is also inherited so that `spawn` expressions inside
    // lambda bodies can derive the correct actor BEAM module name.
    // actor_parent is inherited so that inner lambdas (including the recursive
    // ones the inner-fn surface lowers to) inside an actor handler body keep
    // routing parent-module SymbolRef::Local calls through the qualified
    // `call 'parent':'fn' (args…)` path instead of the bare Apply branch,
    // which would otherwise produce `undefined function fn/n` at erlc time.
    // letrec_locals is inherited for the symmetric reason: a letrec name
    // registered in the outer handler scope must still be recognised as
    // letrec-local (not parent-module) from inside any nested lambda.
    let mut lambda_scope = LocalScope::with_arity_arc(std::sync::Arc::clone(&outer_scope.fn_arity));
    lambda_scope
        .own_module_beam_name
        .clone_from(&outer_scope.own_module_beam_name);
    lambda_scope
        .actor_parent
        .clone_from(&outer_scope.actor_parent);
    lambda_scope.letrec_locals = std::sync::Arc::clone(&outer_scope.letrec_locals);
    // Inherited so a cross-module zero-arity call inside a lambda body keeps the
    // callee-arity information the unit-paren shim needs.
    lambda_scope.external_arity = std::sync::Arc::clone(&outer_scope.external_arity);
    let param_vars: Vec<CErlVar> = params
        .iter()
        .map(|p| CErlVar(name_to_erl_var(&p.name)))
        .collect();
    // §4.9 — Same elide/wrap routing as item-level fn bodies: lambdas that
    // contain guard/early-return patterns need a try/catch frame; tail-only
    // Returns are elided before lowering so no throw is emitted.
    let lowered_body = if crate::return_::has_non_tail_return(body) {
        let body = lower_expr_in_scope(body, &mut lambda_scope)?;
        crate::return_::wrap_with_return_catch(body)
    } else {
        let elided = crate::return_::elide_tail_returns(body);
        lower_expr_in_scope(&elided, &mut lambda_scope)?
    };
    Ok(CErlExpr::Fun {
        params: param_vars,
        body: Box::new(lowered_body),
    })
}

// ── §4.4 Call lowering ────────────────────────────────────────────────────────

/// Lower `IrExpr::Call` to `CErlExpr::Call` (static callee) or
/// `CErlExpr::Apply` (dynamic callee) per §4.4.
///
/// **Static path** — callee is `IrExpr::Symbol`:
/// - `Local { name }` → unqualified Apply or qualified cross-module Call.
/// - `Stdlib { .. }` → bridge map lookup via `lower_call_to_stdlib`.
/// - `External { module, name }` → qualified cross-module `call 'ridge_module_<id>':'name'(args)`.
/// - `Constructor { UnionVariant, name }` with N args → `Tuple([Atom name, A1..AN])`.
///   Empty args → bare `Lit(Atom name)`.
/// - `Constructor { Record, .. }` → defensive `IrShapeMalformed` (records are
///   constructed via `IrExpr::Construct`, not `Call`).
/// - `Prelude { "Some" }` with 1 arg → `Tuple([Atom "some", A0])`.
/// - `Prelude { "None" }` with 0 args → `Lit(Atom "none")`.
/// - `Prelude { "Ok"   }` with 1 arg → `Tuple([Atom "ok",    A0])`.
/// - `Prelude { "Err"  }` with 1 arg → `Tuple([Atom "error", A0])`.
/// - `Prelude { <other> }` → defensive `IrShapeMalformed`.
/// - `Handler { .. }` / `ActorType { .. }` → defensive `IrShapeMalformed`.
///
/// **Dynamic path** — any other callee → `CErlExpr::Apply { callee, args }`.
fn lower_call(
    callee: &IrExpr,
    args: &[IrExpr],
    span: ridge_ast::Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    if let IrExpr::Symbol { sym, .. } = callee {
        // Static callee: dispatch by SymbolRef.
        lower_static_call(sym, args, span, scope)
    } else {
        // Dynamic callee: lower it and emit Apply.
        //
        // B-6 (Phase 6 pass 3): special-case `IrExpr::Local { name }` when we
        // are in actor-handler context and `name` is a parent-module fn/const.
        //
        // Phase 5 may emit `IrExpr::Call { callee: IrExpr::Local { name }, args }`
        // for a call to a module-level function.  In actor context, the actor and
        // its parent are **separate BEAM modules**, so an unqualified `apply
        // 'fnName'/arity ()` in the actor module fails at load time.  When the
        // scope carries `actor_parent` and `name` is in `fn_arity` (confirming it
        // is a module-level symbol, not a handler-scoped local), emit the qualified
        // `call 'parent_beam':'fnName' (args…)` form instead.
        if let IrExpr::Local { name, .. } = callee {
            // Clone early to release the borrow on scope before mutably borrowing
            // scope again in the args-lowering closure.
            let parent_beam_opt: Option<String> = scope
                .actor_parent
                .as_ref()
                // Must be in fn_arity (a module-level fn) AND must NOT be a
                // handler-local letrec function.  Letrec-local names are
                // temporarily registered in fn_arity for self-reference resolution
                // inside the lambda body, but their call sites must remain as
                // unqualified `apply 'fn'/arity (args…)` — not cross-module calls.
                .filter(|_| {
                    scope.fn_arity.contains_key(name.as_str())
                        && !scope.letrec_locals.contains(name.as_str())
                })
                .map(|(_, beam)| beam.to_string());
            if let Some(parent_beam) = parent_beam_opt {
                let lowered_args = args
                    .iter()
                    .map(|a| lower_expr_in_scope(a, scope))
                    .collect::<Result<Vec<_>, _>>()?;
                return Ok(CErlExpr::Call {
                    module: CErlAtom(parent_beam),
                    fn_name: CErlAtom(name.clone()),
                    args: lowered_args,
                });
            }
        }

        let lowered_callee = lower_expr_in_scope(callee, scope)?;
        let lowered_args = args
            .iter()
            .map(|a| lower_expr_in_scope(a, scope))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(CErlExpr::Apply {
            callee: Box::new(lowered_callee),
            args: lowered_args,
        })
    }
}

/// Dispatch a statically-known callee (all `SymbolRef` variants) for a `Call`
/// node.  Split out of [`lower_call`] to keep match arms readable.
fn lower_static_call(
    sym: &SymbolRef,
    args: &[IrExpr],
    span: ridge_ast::Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    match sym {
        // ── Local fn-call → apply 'fnName'/arity (args...). ─────────────────
        // B-6 (Phase 6 pass 3): if the scope carries an actor-parent context and
        // this symbol's `module` matches the parent's `ModuleId`, the symbol lives
        // in the parent BEAM module — emit a qualified `call 'parent':'fn' (args…)`.
        // Actor BEAM modules are separate compilation units and cannot make
        // unqualified calls into their parent module.
        SymbolRef::Local { name, module } => {
            // Unit-paren shim: if the callee is a known 0-arity local fn AND
            // the caller passes a single Unit literal (`f ()`), treat the
            // `()` as syntactic punctuation and emit a 0-arg call.  This
            // unifies the two common decl/call shapes so that user code
            // doesn't have to memorise the difference between
            //   `fn foo ()              -> T = ...`   (parsed: 0 params)
            //   `fn foo (_unit: Unit)   -> T = ...`   (parsed: 1 param)
            // — both can now be called as `foo ()` without erlc rejecting
            // an arity mismatch.  Mirrors the `cli_args/0` ↔ `cli_args/1`
            // shim pattern in `ridge_rt.erl`.
            let effective_args: &[IrExpr] = if args.len() == 1
                && matches!(
                    &args[0],
                    IrExpr::Lit {
                        value: ridge_ir::IrLit::Unit,
                        ..
                    }
                )
                && scope.fn_arity.get(name).copied() == Some(0)
            {
                &[]
            } else {
                args
            };

            let lowered_args = effective_args
                .iter()
                .map(|a| lower_expr_in_scope(a, scope))
                .collect::<Result<Vec<_>, _>>()?;

            // Check if this is a cross-module call from an actor handler.
            if let Some((parent_id, ref parent_beam)) = scope.actor_parent {
                if *module == parent_id {
                    // Qualified inter-module call: call 'parent_module':'fn' (args…).
                    return Ok(CErlExpr::Call {
                        module: CErlAtom(parent_beam.to_string()),
                        fn_name: CErlAtom(name.clone()),
                        args: lowered_args,
                    });
                }
            }

            // Same-module (or non-actor context): unqualified apply.
            #[allow(clippy::cast_possible_truncation)]
            let arity = effective_args.len() as u32;
            Ok(CErlExpr::Apply {
                callee: Box::new(CErlExpr::LocalFnRef {
                    name: CErlAtom(name.clone()),
                    arity,
                }),
                args: lowered_args,
            })
        }

        // ── Stdlib call → bridge map lookup. ─────────────────────────────────
        SymbolRef::Stdlib { module, name } => lower_call_to_stdlib(module, name, args, span, scope),

        // ── External call → qualified cross-module call. ─────────────────────
        // A symbol imported from another user module (or a cross-module instance
        // dictionary). The producer's BEAM module name is mangled from its id,
        // matching the scheme `codegen_one_module` uses; the call arity is the
        // number of arguments.
        SymbolRef::External { name, module } => {
            // Unit-paren shim (cross-module): a zero-arity callee called as
            // `f ()` carries a single Unit arg in the IR, but it compiles to
            // arity 0 in its own module — so passing the Unit would emit an
            // arity-1 call that is `undef` against the arity-0 callee. The
            // workspace-wide arity table recovers the callee's arity across the
            // module boundary; when it is zero, drop the `()` punctuation, the
            // same shim the local-call path applies.
            let callee_is_zero_arity = scope
                .external_arity
                .get(module)
                .and_then(|names| names.get(name.as_str()))
                .copied()
                == Some(0);
            let effective_args: &[IrExpr] = if callee_is_zero_arity
                && args.len() == 1
                && matches!(
                    &args[0],
                    IrExpr::Lit {
                        value: ridge_ir::IrLit::Unit,
                        ..
                    }
                ) {
                &[]
            } else {
                args
            };
            let lowered_args = effective_args
                .iter()
                .map(|a| lower_expr_in_scope(a, scope))
                .collect::<Result<Vec<_>, _>>()?;
            let segment = format!("module_{}", module.0);
            let beam_module = crate::module::mangle_module_name(&[segment.as_str()], *module)?;
            Ok(CErlExpr::Call {
                module: CErlAtom(beam_module),
                fn_name: CErlAtom(name.clone()),
                args: lowered_args,
            })
        }

        // ── UnionVariant constructor call. ────────────────────────────────────
        SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            name,
            ..
        } => {
            if args.is_empty() {
                // Zero-payload: bare atom.
                Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))))
            } else {
                // Tagged tuple: {Name, A1, ..., AN}.
                let mut elems = Vec::with_capacity(args.len() + 1);
                elems.push(CErlExpr::Lit(CErlLit::Atom(CErlAtom(name.clone()))));
                for arg in args {
                    elems.push(lower_expr_in_scope(arg, scope)?);
                }
                Ok(CErlExpr::Tuple(elems))
            }
        }

        // ── Record constructor via Call is a Phase-5 invariant violation. ─────
        SymbolRef::Constructor {
            ctor_kind: CtorKind::Record,
            name,
            ..
        } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Call",
            span,
            detail: format!(
                "Record constructor '{name}' appeared as Call callee; \
                 records are constructed via IrExpr::Construct (Phase 5 invariant violated)"
            ),
        }),

        // ── Prelude constructors. ─────────────────────────────────────────────
        SymbolRef::Prelude { name } => lower_prelude_call(name, args, span, scope),

        // ── Handler/ActorType as a Call callee is a Phase-5 invariant violation.
        SymbolRef::Handler { actor, handler, .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Call",
            span,
            detail: format!(
                "Handler '{actor}/{handler}' appeared as a Call callee; \
                 handlers go through IrExpr::Send/Ask (Phase 5 invariant violated)"
            ),
        }),
        SymbolRef::ActorType { name, .. } => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Call",
            span,
            detail: format!(
                "ActorType '{name}' appeared as a Call callee; \
                 actor types go through IrExpr::Spawn (Phase 5 invariant violated)"
            ),
        }),

        // SymbolRef is #[non_exhaustive]; catch future variants defensively.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Call",
            span,
            detail: "unrecognised SymbolRef variant as Call callee".into(),
        }),
    }
}

/// Maps a prelude `JsonValue` constructor name to its lowercase-snake BEAM
/// atom tag and whether it carries a single payload, mirroring the wire format
/// `ridge_rt:json_*` produces. Returns `None` for non-JSON prelude names.
pub(crate) fn json_ctor_tag(name: &str) -> Option<(&'static str, bool)> {
    Some(match name {
        "JNull" => ("json_null", false),
        "JBool" => ("json_bool", true),
        "JInt" => ("json_int", true),
        "JFloat" => ("json_float", true),
        "JText" => ("json_text", true),
        "JList" => ("json_list", true),
        "JObject" => ("json_object", true),
        _ => return None,
    })
}

/// Lower a `Prelude`-callee call (the `Some/None/Ok/Err` and `JsonValue`
/// dispatch).
fn lower_prelude_call(
    name: &str,
    args: &[IrExpr],
    span: ridge_ast::Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    if let Some((tag, has_payload)) = json_ctor_tag(name) {
        // JsonValue variants → `json_null` / `{json_int, N}` / … BEAM atoms.
        if has_payload {
            if args.len() != 1 {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!("Prelude '{name}' call expects 1 arg, got {}", args.len()),
                });
            }
            let inner = lower_expr_in_scope(&args[0], scope)?;
            return Ok(CErlExpr::Tuple(vec![
                CErlExpr::Lit(CErlLit::Atom(CErlAtom(tag.into()))),
                inner,
            ]));
        }
        if !args.is_empty() {
            return Err(CodegenError::IrShapeMalformed {
                variant: "IrExpr::Call",
                span,
                detail: format!("Prelude '{name}' call expects 0 args, got {}", args.len()),
            });
        }
        return Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(tag.into()))));
    }
    match name {
        "None" => {
            if !args.is_empty() {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!("Prelude 'None' call expects 0 args, got {}", args.len()),
                });
            }
            Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom("none".into()))))
        }
        "Some" => {
            if args.len() != 1 {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!("Prelude 'Some' call expects 1 arg, got {}", args.len()),
                });
            }
            let inner = lower_expr_in_scope(&args[0], scope)?;
            Ok(CErlExpr::Tuple(vec![
                CErlExpr::Lit(CErlLit::Atom(CErlAtom("some".into()))),
                inner,
            ]))
        }
        "Ok" => {
            if args.len() != 1 {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!("Prelude 'Ok' call expects 1 arg, got {}", args.len()),
                });
            }
            let inner = lower_expr_in_scope(&args[0], scope)?;
            Ok(CErlExpr::Tuple(vec![
                CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
                inner,
            ]))
        }
        "Err" => {
            if args.len() != 1 {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!("Prelude 'Err' call expects 1 arg, got {}", args.len()),
                });
            }
            let inner = lower_expr_in_scope(&args[0], scope)?;
            Ok(CErlExpr::Tuple(vec![
                CErlExpr::Lit(CErlLit::Atom(CErlAtom("error".into()))),
                inner,
            ]))
        }
        other => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Call",
            span,
            detail: format!(
                "Prelude '{other}' is not a valid Call callee — Phase 5 invariant violated"
            ),
        }),
    }
}

/// Read the declared arity out of any [`BridgeTarget`] variant.  Used by
/// `lower_call_to_stdlib` to apply the 0-arity `Unit`-drop shim before the
/// per-variant arity check.
const fn bridge_target_arity(target: &stdlib_map::BridgeTarget) -> u32 {
    use stdlib_map::BridgeTarget;
    match target {
        BridgeTarget::BeamStdlib { arity, .. }
        | BridgeTarget::BeamStdlibPerm { arity, .. }
        | BridgeTarget::RidgeRuntime { arity, .. }
        | BridgeTarget::RidgeStdlibLocal { arity, .. } => *arity,
    }
}

// ── Stdlib call dispatch (§4.4 / §3.4) ──────────────────────────────────────

/// Lower a `Call { callee: Symbol(Stdlib { module, name }), args }` node using
/// the static bridge map.
///
/// # Identity shortcuts
///
/// - `std.text.toText` — identity on `Text`; erased at codegen. Always 1 arg.
///
/// # Bridge dispatch
///
/// All other symbols are routed through [`stdlib_map::lookup`].  A `None` result
/// produces `E002 StdlibBridgeMissing`.
// The `_` arm below is a defensive catch for future `#[non_exhaustive]`
// BridgeTarget variants added in Phase 7+.  Inside this crate all current
// variants are exhaustive, so the compiler warns; suppress it here.
#[allow(unreachable_patterns)]
pub(crate) fn lower_call_to_stdlib(
    module: &str,
    name: &str,
    args: &[IrExpr],
    span: ridge_ast::Span,
    scope: &mut LocalScope,
) -> Result<CErlExpr, CodegenError> {
    // ── Identity shortcuts ────────────────────────────────────────────────────
    // `std.net.http.respond` is intentionally NOT shortcut here: its signature
    // is `(Int, Text) -> Response` (constructs a record), so the codegen must
    // route it through the regular bridge so the compiled `respond/2` runs
    // and returns a Response — not the integer status code by itself.
    if (module, name) == ("std.text", "toText") {
        if args.len() != 1 {
            return Err(CodegenError::IrShapeMalformed {
                variant: "IrExpr::Call",
                span,
                detail: format!(
                    "stdlib identity shortcut '{module}.{name}' expects 1 arg, got {}",
                    args.len()
                ),
            });
        }
        return lower_expr_in_scope(&args[0], scope);
    }

    // ── Bridge map lookup ─────────────────────────────────────────────────────
    let Some(target) = stdlib_map::lookup(module, name) else {
        return Err(CodegenError::StdlibBridgeMissing {
            module: module.into(),
            name: name.into(),
            span,
        });
    };

    // Drop a single `Unit` literal arg when the bridge target is 0-arity so
    // user code like `Map.empty ()` or `Json.jNull ()` lowers as a 0-arg call
    // (mirrors the local-fn shim from PR #71 on Ridge-side calls).
    // Without this, the parser-supplied `[Unit]` argument list trips every
    // per-variant arity check below with `expects 0 args, got 1`.
    let args: &[IrExpr] = if bridge_target_arity(target) == 0
        && args.len() == 1
        && matches!(
            &args[0],
            IrExpr::Lit {
                value: IrLit::Unit,
                ..
            }
        ) {
        &[]
    } else {
        args
    };

    match target {
        BridgeTarget::BeamStdlib {
            module: m,
            fn_name,
            arity,
        } => {
            if args.len() != *arity as usize {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!(
                        "stdlib call '{module}.{name}' expects {arity} args, got {}",
                        args.len()
                    ),
                });
            }
            let lowered_args = args
                .iter()
                .map(|a| lower_expr_in_scope(a, scope))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CErlExpr::Call {
                module: CErlAtom((*m).into()),
                fn_name: CErlAtom((*fn_name).into()),
                args: lowered_args,
            })
        }

        BridgeTarget::BeamStdlibPerm {
            module: m,
            fn_name,
            arity,
            perm,
        } => {
            if args.len() != *arity as usize {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!(
                        "stdlib perm-call '{module}.{name}' expects {arity} args, got {}",
                        args.len()
                    ),
                });
            }
            if perm.len() != *arity as usize {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!(
                        "stdlib perm-call '{module}.{name}': perm length {} != arity {arity}",
                        perm.len()
                    ),
                });
            }
            // Lower source args in order first.
            let lowered_source = args
                .iter()
                .map(|a| lower_expr_in_scope(a, scope))
                .collect::<Result<Vec<_>, _>>()?;
            // Reorder: emitted arg i = source arg perm[i].
            let mut emitted = Vec::with_capacity(lowered_source.len());
            for &src_idx in *perm {
                emitted.push(lowered_source[src_idx as usize].clone());
            }
            Ok(CErlExpr::Call {
                module: CErlAtom((*m).into()),
                fn_name: CErlAtom((*fn_name).into()),
                args: emitted,
            })
        }

        BridgeTarget::RidgeRuntime { fn_name, arity } => {
            if args.len() != *arity as usize {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!(
                        "stdlib runtime-call '{module}.{name}' expects {arity} args, got {}",
                        args.len()
                    ),
                });
            }
            let lowered_args = args
                .iter()
                .map(|a| lower_expr_in_scope(a, scope))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CErlExpr::Call {
                module: CErlAtom("ridge_rt".into()),
                fn_name: CErlAtom((*fn_name).into()),
                args: lowered_args,
            })
        }

        // Phase 7: RidgeStdlibLocal — emit a direct BEAM call (beam_module:fn_name/arity).
        // The Ridge stdlib source declares the arity; enforce it here the same way as BeamStdlib.
        BridgeTarget::RidgeStdlibLocal {
            beam_module,
            fn_name,
            arity,
        } => {
            if args.len() != *arity as usize {
                return Err(CodegenError::IrShapeMalformed {
                    variant: "IrExpr::Call",
                    span,
                    detail: format!(
                        "stdlib local-call '{module}.{name}' expects {arity} args, got {}",
                        args.len()
                    ),
                });
            }
            let lowered_args = args
                .iter()
                .map(|a| lower_expr_in_scope(a, scope))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(CErlExpr::Call {
                module: CErlAtom(beam_module.clone()),
                fn_name: CErlAtom(fn_name.clone()),
                args: lowered_args,
            })
        }

        // #[non_exhaustive] catch: future BridgeTarget variants.
        _ => Err(CodegenError::IrShapeMalformed {
            variant: "IrExpr::Call",
            span,
            detail: "unrecognised BridgeTarget variant".into(),
        }),
    }
}

// ── LetIn recursive inner-fn detection (OQ-L012) ────────────────────────────
// Moved to letrec_detect.rs; imported at the top of this file.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit, CErlVar};
    use ridge_ast::Span;
    use ridge_ir::{
        AssignTarget, CapabilitySet, CtorKind, IrArm, IrExpr, IrLit, IrNodeId, IrParam, IrPat,
        SymbolRef,
    };
    use ridge_types::{TyConId, Type};

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

    fn lit_text(s: &str) -> IrExpr {
        IrExpr::Lit {
            id: node(),
            value: IrLit::Text(s.into()),
            span: sp(),
        }
    }

    fn local(name: &str) -> IrExpr {
        IrExpr::Local {
            id: node(),
            name: name.into(),
            span: sp(),
        }
    }

    // ── name_to_erl_var mangler tests ────────────────────────────────────────

    #[test]
    fn mangler_simple() {
        // §4.2: "count" → "V_Count"
        assert_eq!(name_to_erl_var("count"), "V_Count");
    }

    #[test]
    fn mangler_prop_ok() {
        // §4.2 Phase-5-synthesised name: "__prop_ok" → "V_PropOk"
        assert_eq!(name_to_erl_var("__prop_ok"), "V_PropOk");
    }

    #[test]
    fn mangler_with_base() {
        // §4.2 Phase-5-synthesised name: "__with_base" → "V_WithBase"
        assert_eq!(name_to_erl_var("__with_base"), "V_WithBase");
    }

    #[test]
    fn mangler_single_word_no_underscore() {
        assert_eq!(name_to_erl_var("x"), "V_X");
    }

    #[test]
    fn mangler_multi_segment() {
        assert_eq!(name_to_erl_var("my_var_name"), "V_MyVarName");
    }

    // ── lower_expr: Lit ──────────────────────────────────────────────────────

    #[test]
    fn expr_lit_int() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::Int(42),
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Int(42)))));
    }

    #[test]
    fn expr_lit_float() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::Float(1.5),
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(
            matches!(result, Ok(CErlExpr::Lit(CErlLit::Float(f))) if f.to_bits() == 1.5_f64.to_bits())
        );
    }

    #[test]
    fn expr_lit_bool_true() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::Bool(true),
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s)))) if s == "true"));
    }

    #[test]
    fn expr_lit_bool_false() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::Bool(false),
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(
            matches!(result, Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s)))) if s == "false")
        );
    }

    #[test]
    fn expr_lit_text() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::Text("hi".into()),
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Binary(ref b))) if b == b"hi"));
    }

    #[test]
    fn expr_lit_unit() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::Unit,
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s)))) if s == "ok"));
    }

    #[test]
    fn expr_lit_empty_list() {
        let expr = IrExpr::Lit {
            id: node(),
            value: IrLit::EmptyList,
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(matches!(result, Ok(CErlExpr::Lit(CErlLit::Nil))));
    }

    // ── lower_expr: Local ────────────────────────────────────────────────────

    #[test]
    fn expr_local_mangled() {
        let expr = IrExpr::Local {
            id: node(),
            name: "count".into(),
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(matches!(result, Ok(CErlExpr::Var(CErlVar(ref s))) if s == "V_Count"));
    }

    // ── lower_expr: Tuple ────────────────────────────────────────────────────

    #[test]
    fn expr_tuple_two_ints() {
        // §4.15: Tuple {elems: [Int 1, Int 2]} → CErlExpr::Tuple([Lit Int 1, Lit Int 2])
        let expr = IrExpr::Tuple {
            id: node(),
            elems: vec![lit_int(1), lit_int(2)],
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Ok(CErlExpr::Tuple(elems)) => {
                assert_eq!(elems.len(), 2);
                assert!(matches!(elems[0], CErlExpr::Lit(CErlLit::Int(1))));
                assert!(matches!(elems[1], CErlExpr::Lit(CErlLit::Int(2))));
            }
            other => panic!("expected CErlExpr::Tuple, got {other:?}"),
        }
    }

    // ── lower_expr: ListLit ──────────────────────────────────────────────────

    #[test]
    fn expr_list_lit_single_elem() {
        // §4.14: ListLit {elems: [Int 1]} → CErlExpr::ListLit([Lit Int 1])
        let expr = IrExpr::ListLit {
            id: node(),
            elems: vec![lit_int(1)],
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Ok(CErlExpr::ListLit(elems)) => {
                assert_eq!(elems.len(), 1);
                assert!(matches!(elems[0], CErlExpr::Lit(CErlLit::Int(1))));
            }
            other => panic!("expected CErlExpr::ListLit, got {other:?}"),
        }
    }

    // ── lower_expr: Cons ─────────────────────────────────────────────────────

    #[test]
    fn expr_cons_head_tail() {
        // §4.16: Cons {head: Int 1, tail: EmptyList} → CErlExpr::Cons { ... }
        let expr = IrExpr::Cons {
            id: node(),
            head: Box::new(lit_int(1)),
            tail: Box::new(IrExpr::Lit {
                id: node(),
                value: IrLit::EmptyList,
                span: sp(),
            }),
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Ok(CErlExpr::Cons { head, tail }) => {
                assert!(matches!(*head, CErlExpr::Lit(CErlLit::Int(1))));
                assert!(matches!(*tail, CErlExpr::Lit(CErlLit::Nil)));
            }
            other => panic!("expected CErlExpr::Cons, got {other:?}"),
        }
    }

    // ── Call with dynamic callee (Local) → Apply ─────────────────────────────
    // A dynamic callee (anything other than IrExpr::Symbol) lowers to Apply.

    #[test]
    fn expr_call_dynamic_local_callee_emits_apply() {
        // Call { callee: Local "f", args: [] } → Apply { callee: Var V_F, args: [] }
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(local("f")),
            args: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Ok(CErlExpr::Apply { callee, args }) => {
                assert!(matches!(*callee, CErlExpr::Var(CErlVar(ref s)) if s == "V_F"));
                assert!(args.is_empty());
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    // ── LetIn (simple bind) ───────────────────────────────────────────────────

    #[test]
    fn expr_let_in_simple_bind() {
        // LetIn { pat: Bind("x", None), value: Lit Int 1, body: Local "x" }
        // → Let { V_X, Lit Int 1, Var V_X }
        let expr = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Bind {
                name: "x".into(),
                inner: None,
                span: sp(),
            },
            value: Box::new(lit_int(1)),
            body: Box::new(local("x")),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Let { var, value, body } => {
                assert_eq!(var.0, "V_X");
                assert!(matches!(*value, CErlExpr::Lit(CErlLit::Int(1))));
                assert!(matches!(*body, CErlExpr::Var(CErlVar(ref s)) if s == "V_X"));
            }
            other => panic!("expected Let, got {other:?}"),
        }
    }

    // ── LetIn (destructuring) ────────────────────────────────────────────────

    #[test]
    fn expr_let_in_destructuring() {
        // LetIn { pat: Tuple([Bind "a", Bind "b"]), value: Tuple([Int 1, Int 2]),
        //         body: Local "a" }
        // → Case { Tuple([Int 1, Int 2]), [{ Tuple([V_A, V_B]), 'true', V_A }] }
        let expr = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Tuple {
                elems: vec![
                    IrPat::Bind {
                        name: "a".into(),
                        inner: None,
                        span: sp(),
                    },
                    IrPat::Bind {
                        name: "b".into(),
                        inner: None,
                        span: sp(),
                    },
                ],
                span: sp(),
            },
            value: Box::new(IrExpr::Tuple {
                id: node(),
                elems: vec![lit_int(1), lit_int(2)],
                span: sp(),
            }),
            body: Box::new(local("a")),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Case { scrutinee, clauses } => {
                assert_eq!(clauses.len(), 1);
                assert!(matches!(
                    *scrutinee,
                    CErlExpr::Tuple(ref elems) if elems.len() == 2
                ));
                let clause = &clauses[0];
                assert!(
                    matches!(&clause.guard, CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "true")
                );
                assert!(matches!(&clause.body, CErlExpr::Var(CErlVar(ref s)) if s == "V_A"));
            }
            other => panic!("expected Case, got {other:?}"),
        }
    }

    // ── Nested LetIn chain ────────────────────────────────────────────────────

    #[test]
    fn expr_let_in_nested() {
        // LetIn "a" 1 (LetIn "b" 2 (LetIn "c" 3 (Local "c")))
        // → Let V_A=1 in Let V_B=2 in Let V_C=3 in Var V_C
        let expr = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Bind {
                name: "a".into(),
                inner: None,
                span: sp(),
            },
            value: Box::new(lit_int(1)),
            body: Box::new(IrExpr::LetIn {
                id: node(),
                pat: IrPat::Bind {
                    name: "b".into(),
                    inner: None,
                    span: sp(),
                },
                value: Box::new(lit_int(2)),
                body: Box::new(IrExpr::LetIn {
                    id: node(),
                    pat: IrPat::Bind {
                        name: "c".into(),
                        inner: None,
                        span: sp(),
                    },
                    value: Box::new(lit_int(3)),
                    body: Box::new(local("c")),
                    span: sp(),
                }),
                span: sp(),
            }),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        // Outermost must be Let V_A.
        match &result {
            CErlExpr::Let { var, value, body } => {
                assert_eq!(var.0, "V_A");
                assert!(matches!(**value, CErlExpr::Lit(CErlLit::Int(1))));
                // Inner should be Let V_B.
                match body.as_ref() {
                    CErlExpr::Let {
                        var: v2,
                        value: val2,
                        body: b2,
                    } => {
                        assert_eq!(v2.0, "V_B");
                        assert!(matches!(**val2, CErlExpr::Lit(CErlLit::Int(2))));
                        // Innermost should be Let V_C.
                        match b2.as_ref() {
                            CErlExpr::Let { var: v3, .. } => assert_eq!(v3.0, "V_C"),
                            other => panic!("expected Let V_C, got {other:?}"),
                        }
                    }
                    other => panic!("expected Let V_B, got {other:?}"),
                }
            }
            other => panic!("expected Let V_A, got {other:?}"),
        }
    }

    // ── VarIn + Assign in Block ───────────────────────────────────────────────

    #[test]
    fn expr_var_in_then_assign_in_block() {
        // Simulates:
        //   var n = 0
        //   n <- 5
        //   n
        //
        // VarIn introduces n at index 0 (V_N).
        // The body is Block { stmts: [Assign Local "n" Lit 5, Local "n"] }.
        // After the Assign, n is at index 1 (V_N1).
        // Local "n" resolves to V_N1.
        let inner_block = IrExpr::Block {
            id: node(),
            stmts: vec![
                IrExpr::Assign {
                    id: node(),
                    target: AssignTarget::Local {
                        name: "n".into(),
                        span: sp(),
                    },
                    value: Box::new(lit_int(5)),
                    span: sp(),
                },
                local("n"),
            ],
            span: sp(),
        };
        let expr = IrExpr::VarIn {
            id: node(),
            name: "n".into(),
            ty: ridge_types::Type::Error,
            value: Box::new(lit_int(0)),
            body: Box::new(inner_block),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        // Should be: Let V_N = 0 in Let V_N1 = 5 in Var V_N1
        match result {
            CErlExpr::Let { var, value, body } => {
                assert_eq!(var.0, "V_N", "outer let should bind V_N (index 0)");
                assert!(matches!(*value, CErlExpr::Lit(CErlLit::Int(0))));
                match *body {
                    CErlExpr::Let {
                        var: v2,
                        value: val2,
                        body: b2,
                    } => {
                        assert_eq!(v2.0, "V_N1", "assign let should bind V_N1 (index 1)");
                        assert!(matches!(*val2, CErlExpr::Lit(CErlLit::Int(5))));
                        assert!(
                            matches!(*b2, CErlExpr::Var(CErlVar(ref s)) if s == "V_N1"),
                            "local 'n' should resolve to V_N1 after assign"
                        );
                    }
                    other => panic!("expected inner Let, got {other:?}"),
                }
            }
            other => panic!("expected outer Let, got {other:?}"),
        }
    }

    // ── Block Do sequencing ───────────────────────────────────────────────────

    #[test]
    fn expr_block_do_sequencing() {
        // Block { stmts: [Lit Unit, Lit Unit] } → Do { Lit 'ok', Lit 'ok' }
        let expr = IrExpr::Block {
            id: node(),
            stmts: vec![
                IrExpr::Lit {
                    id: node(),
                    value: IrLit::Unit,
                    span: sp(),
                },
                IrExpr::Lit {
                    id: node(),
                    value: IrLit::Unit,
                    span: sp(),
                },
            ],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Do { first, then } => {
                assert!(
                    matches!(*first, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "ok")
                );
                assert!(
                    matches!(*then, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "ok")
                );
            }
            other => panic!("expected Do, got {other:?}"),
        }
    }

    // ── Match (two arms) ──────────────────────────────────────────────────────

    #[test]
    fn expr_match_with_two_arms() {
        // Match { scrutinee: Local "x",
        //   arms: [Wild → Int 0, Lit Int 1 → Int 2] }
        // → Case { Var V_X, [{ Wild, 'true', Int 0 }, { Lit Int 1, 'true', Int 2 }] }
        let expr = IrExpr::Match {
            id: node(),
            scrutinee: Box::new(local("x")),
            arms: vec![
                IrArm {
                    pat: IrPat::Wild { span: sp() },
                    when: None,
                    body: lit_int(0),
                    span: sp(),
                },
                IrArm {
                    pat: IrPat::Lit {
                        value: IrLit::Int(1),
                        span: sp(),
                    },
                    when: None,
                    body: lit_int(2),
                    span: sp(),
                },
            ],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Case { scrutinee, clauses } => {
                assert!(matches!(*scrutinee, CErlExpr::Var(CErlVar(ref s)) if s == "V_X"));
                assert_eq!(clauses.len(), 2);
                // First arm: Wild → Int 0
                assert!(matches!(
                    &clauses[0].pattern,
                    crate::core_ast::CErlPat::Wild
                ));
                assert!(matches!(clauses[0].body, CErlExpr::Lit(CErlLit::Int(0))));
                // Second arm: Lit Int 1 → Int 2
                assert!(matches!(
                    &clauses[1].pattern,
                    crate::core_ast::CErlPat::Lit(CErlLit::Int(1))
                ));
                assert!(matches!(clauses[1].body, CErlExpr::Lit(CErlLit::Int(2))));
            }
            other => panic!("expected Case, got {other:?}"),
        }
    }

    // ── Match over Record Ctor pattern ───────────────────────────────────────

    #[test]
    fn expr_match_over_record_ctor_pattern() {
        // Match scrutinee over IrPat::Ctor { Record, fields: [("x", Wild)] }
        // → Case with MapPat clause.
        use crate::core_ast::CErlPat;
        let expr = IrExpr::Match {
            id: node(),
            scrutinee: Box::new(local("r")),
            arms: vec![IrArm {
                pat: IrPat::Ctor {
                    sym: SymbolRef::Constructor {
                        ctor_kind: CtorKind::Record,
                        owner_type: TyConId(0),
                        name: "Point".into(),
                        variant: 0,
                    },
                    fields: vec![("x".into(), IrPat::Wild { span: sp() })],
                    args: vec![],
                    span: sp(),
                },
                when: None,
                body: lit_int(42),
                span: sp(),
            }],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Case { clauses, .. } => {
                assert_eq!(clauses.len(), 1);
                assert!(matches!(&clauses[0].pattern, CErlPat::MapPat(_)));
            }
            other => panic!("expected Case, got {other:?}"),
        }
    }

    // ── Match over UnionVariant Ctor pattern ─────────────────────────────────

    #[test]
    fn expr_match_over_union_variant_pattern() {
        // Match over Ctor { UnionVariant "Foo", args: [Wild] }
        // → Case with Tuple([Atom "Foo", Wild]) clause pattern.
        use crate::core_ast::CErlPat;
        let expr = IrExpr::Match {
            id: node(),
            scrutinee: Box::new(local("v")),
            arms: vec![IrArm {
                pat: IrPat::Ctor {
                    sym: SymbolRef::Constructor {
                        ctor_kind: CtorKind::UnionVariant,
                        owner_type: TyConId(1),
                        name: "Foo".into(),
                        variant: 0,
                    },
                    fields: vec![],
                    args: vec![IrPat::Wild { span: sp() }],
                    span: sp(),
                },
                when: None,
                body: lit_int(0),
                span: sp(),
            }],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Case { clauses, .. } => {
                assert_eq!(clauses.len(), 1);
                match &clauses[0].pattern {
                    CErlPat::Tuple(elems) => {
                        assert_eq!(elems.len(), 2);
                        assert!(
                            matches!(&elems[0], CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "Foo")
                        );
                        assert!(matches!(elems[1], CErlPat::Wild));
                    }
                    other => panic!("expected Tuple pattern, got {other:?}"),
                }
            }
            other => panic!("expected Case, got {other:?}"),
        }
    }

    // ── Return emits throw at expression scope ────────────────────────────────

    #[test]
    fn expr_return_not_tail_emits_throw() {
        // Return { value: Lit Int 5 } → Call { erlang:throw, [{ridge_return, 5}] }
        let expr = IrExpr::Return {
            id: node(),
            value: Box::new(lit_int(5)),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Call {
                module,
                fn_name,
                args,
            } => {
                assert_eq!(module.0, "erlang");
                assert_eq!(fn_name.0, "throw");
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], CErlExpr::Tuple(elems) if elems.len() == 2));
            }
            other => panic!("expected Call(erlang:throw), got {other:?}"),
        }
    }

    // ── Assign StateField requires actor-handler context ─────────────────────

    #[test]
    fn expr_assign_state_field_deferred() {
        let expr = IrExpr::Assign {
            id: node(),
            target: AssignTarget::StateField {
                name: "count".into(),
                span: sp(),
            },
            value: Box::new(lit_int(0)),
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Err(CodegenError::IrShapeMalformed { detail, .. }) => {
                assert!(
                    detail.contains("StateField Assign requires actor-handler context"),
                    "unexpected detail: {detail}"
                );
            }
            other => panic!("expected IrShapeMalformed, got {other:?}"),
        }
    }

    /// When the same `StateField` assign is reached from inside a scope that
    /// IS within an actor handler (i.e. `actor_parent` is set, which is what
    /// `lower_lambda` inherits when lowering a `fn` declared inside a
    /// handler body), the error must call out the lambda-nesting cause and
    /// point at the canonical workaround (extract loop to top-level fn
    /// returning an accumulator record).  The plain "requires actor-handler
    /// context" phrasing pointed at the wrong fix — the assign IS reached
    /// from an actor context, just not the top-level handler one.
    #[test]
    fn expr_assign_state_field_inside_handler_lambda_hints_at_workaround() {
        use crate::scope::LocalScope;
        use rustc_hash::FxHashMap;
        let expr = IrExpr::Assign {
            id: node(),
            target: AssignTarget::StateField {
                name: "count".into(),
                span: sp(),
            },
            value: Box::new(lit_int(0)),
            span: sp(),
        };
        let mut scope = LocalScope::with_actor_parent(
            FxHashMap::default(),
            ridge_resolve::ModuleId(0),
            "ridge_module_0",
        );
        let result = lower_expr_in_scope(&expr, &mut scope);
        match result {
            Err(CodegenError::IrShapeMalformed { detail, .. }) => {
                assert!(
                    detail.contains("nested `fn`") || detail.contains("nested fn"),
                    "expected hint about nested fn, got: {detail}"
                );
                assert!(
                    detail.contains("count"),
                    "expected state field name in detail, got: {detail}"
                );
                assert!(
                    detail.contains("Workaround"),
                    "expected workaround pointer in detail, got: {detail}"
                );
            }
            other => panic!("expected IrShapeMalformed, got {other:?}"),
        }
    }

    // ── body_references_local detection (OQ-L012) ────────────────────────────

    #[test]
    fn body_references_local_detects_self_ref() {
        // Block { stmts: [Local "f"] } references "f".
        let body = IrExpr::Block {
            id: node(),
            stmts: vec![local("f")],
            span: sp(),
        };
        assert!(body_references_local(&body, "f"));
        assert!(!body_references_local(&body, "g"));
    }

    #[test]
    fn body_references_local_in_nested_letin() {
        // LetIn that binds a different name but body references target.
        let body = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Bind {
                name: "tmp".into(),
                inner: None,
                span: sp(),
            },
            value: Box::new(lit_int(1)),
            body: Box::new(local("f")),
            span: sp(),
        };
        assert!(body_references_local(&body, "f"));
    }

    // ── IrExpr::Construct — Record → MapLit ──────────────────────────────────

    #[test]
    fn expr_construct_record_emits_map_lit() {
        // Construct { ctor: Record, fields: [("x", Int 1), ("y", Int 2)] }
        // → MapLit [(Atom "x", Int 1), (Atom "y", Int 2)]
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: TyConId(0),
                name: "Point".into(),
                variant: 0,
            },
            fields: vec![("x".into(), lit_int(1)), ("y".into(), lit_int(2))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::MapLit(pairs) => {
                assert_eq!(pairs.len(), 2);
                assert!(
                    matches!(&pairs[0].0, CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "x")
                );
                assert!(matches!(&pairs[0].1, CErlExpr::Lit(CErlLit::Int(1))));
                assert!(
                    matches!(&pairs[1].0, CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "y")
                );
                assert!(matches!(&pairs[1].1, CErlExpr::Lit(CErlLit::Int(2))));
            }
            other => panic!("expected MapLit, got {other:?}"),
        }
    }

    // ── IrExpr::Construct — UnionVariant with payload → Tuple ────────────────

    #[test]
    fn expr_construct_union_with_payload_emits_tuple() {
        // Construct { ctor: UnionVariant "Foo", fields: [("0", Int 1)] }
        // → Tuple([Atom "Foo", Int 1])
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::UnionVariant,
                owner_type: TyConId(0),
                name: "Foo".into(),
                variant: 0,
            },
            fields: vec![("0".into(), lit_int(1))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "Foo")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(1))));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    // ── IrExpr::Construct — UnionVariant zero payload → bare atom ────────────

    #[test]
    fn expr_construct_union_zero_payload_emits_atom() {
        // Construct { ctor: UnionVariant "Bar", fields: [] }
        // → Lit(Atom "Bar")
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::UnionVariant,
                owner_type: TyConId(0),
                name: "Bar".into(),
                variant: 1,
            },
            fields: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        assert!(
            matches!(result, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "Bar"),
            "expected Lit(Atom 'Bar'), got {result:?}"
        );
    }

    // ── IrExpr::Construct — Prelude "Some" → {some, v} ──────────────────────

    #[test]
    fn expr_construct_prelude_some_emits_tuple() {
        // Construct { ctor: Prelude "Some", fields: [("0", Int 7)] }
        // → Tuple([Atom "some", Int 7])
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude {
                name: "Some".into(),
            },
            fields: vec![("0".into(), lit_int(7))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "some")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(7))));
            }
            other => panic!("expected Tuple([some, 7]), got {other:?}"),
        }
    }

    // ── IrExpr::Construct — Prelude "None" → none atom ───────────────────────

    #[test]
    fn expr_construct_prelude_none_emits_atom() {
        // Construct { ctor: Prelude "None", fields: [] } → Lit(Atom "none")
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude {
                name: "None".into(),
            },
            fields: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        assert!(
            matches!(result, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "none"),
            "expected Lit(Atom 'none'), got {result:?}"
        );
    }

    // ── IrExpr::Construct — Prelude JsonValue variants ──────────────────────

    #[test]
    fn expr_construct_prelude_jint_emits_json_int_tuple() {
        // Construct { ctor: Prelude "JInt", fields: [("0", Int 42)] }
        // → Tuple([Atom "json_int", Int 42])
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude {
                name: "JInt".into(),
            },
            fields: vec![("0".into(), lit_int(42))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "json_int")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(42))));
            }
            other => panic!("expected Tuple([json_int, 42]), got {other:?}"),
        }
    }

    #[test]
    fn expr_construct_prelude_jnull_emits_atom() {
        // Construct { ctor: Prelude "JNull", fields: [] } → Lit(Atom "json_null")
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude {
                name: "JNull".into(),
            },
            fields: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        assert!(
            matches!(result, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "json_null"),
            "expected Lit(Atom 'json_null'), got {result:?}"
        );
    }

    // ── IrExpr::Construct — Prelude "Ok" → {ok, v} ──────────────────────────

    #[test]
    fn expr_construct_prelude_ok_emits_tuple() {
        // Construct { ctor: Prelude "Ok", fields: [("0", Int 3)] }
        // → Tuple([Atom "ok", Int 3])
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude { name: "Ok".into() },
            fields: vec![("0".into(), lit_int(3))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "ok")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(3))));
            }
            other => panic!("expected Tuple([ok, 3]), got {other:?}"),
        }
    }

    // ── IrExpr::Construct — Prelude "Err" → {error, v} ──────────────────────

    #[test]
    fn expr_construct_prelude_err_emits_tuple() {
        // Construct { ctor: Prelude "Err", fields: [("0", Int 5)] }
        // → Tuple([Atom "error", Int 5])
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude { name: "Err".into() },
            fields: vec![("0".into(), lit_int(5))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "error")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(5))));
            }
            other => panic!("expected Tuple([error, 5]), got {other:?}"),
        }
    }

    // ── IrExpr::Construct — Prelude "Some" arity mismatch → error ────────────

    #[test]
    fn expr_construct_prelude_some_arity_mismatch_errs() {
        // Construct { ctor: Prelude "Some", fields: [] } — zero fields, expects 1 → error
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Prelude {
                name: "Some".into(),
            },
            fields: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Err(CodegenError::IrShapeMalformed {
                variant, detail, ..
            }) => {
                assert_eq!(variant, "SymbolRef::Prelude");
                assert!(
                    detail.contains("Some") && detail.contains('1') && detail.contains('0'),
                    "unexpected detail: {detail}"
                );
            }
            other => panic!("expected IrShapeMalformed, got {other:?}"),
        }
    }

    // ── IrExpr::Field → call 'maps':'get'(Atom key, Base) ───────────────────

    #[test]
    fn expr_field_emits_maps_get_call() {
        // Field { base: Local "r", field: "x" }
        // → Call { module: "maps", fn_name: "get", args: [Atom "x", Var V_R] }
        let expr = IrExpr::Field {
            id: node(),
            base: Box::new(local("r")),
            field: "x".into(),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Call {
                module,
                fn_name,
                args,
            } => {
                assert_eq!(module.0, "maps");
                assert_eq!(fn_name.0, "get");
                assert_eq!(args.len(), 2);
                // args[0] = key atom
                assert!(
                    matches!(&args[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "x"),
                    "expected key atom 'x', got {:?}",
                    &args[0]
                );
                // args[1] = base variable
                assert!(
                    matches!(&args[1], CErlExpr::Var(CErlVar(s)) if s == "V_R"),
                    "expected base var V_R, got {:?}",
                    &args[1]
                );
            }
            other => panic!("expected Call(maps:get), got {other:?}"),
        }
    }

    // ── `with` over an opaque base → maps:merge/2 ────────────────────────────

    #[test]
    fn expr_record_update_over_var_base_lowers_to_maps_merge() {
        // `r with { b = 99 }` where `r` is a variable (e.g. a function
        // parameter) → IrExpr::RecordUpdate { base: Local("r"), updates:
        //   [("b", Int 99)] } → call 'maps':'merge'(V_R, ~{'b'=>99}~).
        //
        // The native `put_map_assoc` would fail the +5 validator here because
        // a variable has type `any`, so the conversion routes opaque bases
        // through `maps:merge/2`. No record schema is involved.
        let expr = IrExpr::RecordUpdate {
            id: node(),
            base: Box::new(local("r")),
            updates: vec![("b".into(), lit_int(99))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Call {
                module,
                fn_name,
                args,
            } => {
                assert_eq!(module.0, "maps");
                assert_eq!(fn_name.0, "merge");
                assert_eq!(args.len(), 2);
                assert!(
                    matches!(&args[0], CErlExpr::Var(CErlVar(s)) if s == "V_R"),
                    "expected base var V_R, got {:?}",
                    &args[0]
                );
                match &args[1] {
                    CErlExpr::MapLit(pairs) => {
                        assert_eq!(pairs.len(), 1);
                        assert!(
                            matches!(&pairs[0].0, CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "b"),
                            "expected key atom 'b', got {:?}",
                            &pairs[0].0
                        );
                        assert!(
                            matches!(&pairs[0].1, CErlExpr::Lit(CErlLit::Int(99))),
                            "expected value Int 99, got {:?}",
                            &pairs[0].1
                        );
                    }
                    other => panic!("expected MapLit update arg, got {other:?}"),
                }
            }
            other => panic!("expected Call(maps:merge), got {other:?}"),
        }
    }

    // ── `with` over a record literal → native MapUpdate ──────────────────────

    #[test]
    fn expr_record_update_over_record_literal_uses_native_map_update() {
        // `(Point { a = 1 }) with { b = 99 }` → the base lowers to a MapLit,
        // which the BEAM analyser already sees as a map, so the conversion
        // keeps the native map-update `~{'b'=>99 | ~{'a'=>1}~}~`.
        let base = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: TyConId(0),
                name: "Point".into(),
                variant: 0,
            },
            fields: vec![("a".into(), lit_int(1))],
            span: sp(),
        };
        let expr = IrExpr::RecordUpdate {
            id: node(),
            base: Box::new(base),
            updates: vec![("b".into(), lit_int(99))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::MapUpdate { base, updates } => {
                assert!(
                    matches!(*base, CErlExpr::MapLit(_)),
                    "expected MapLit base, got {base:?}"
                );
                assert_eq!(updates.len(), 1);
                assert!(
                    matches!(&updates[0].0, CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "b"),
                    "expected key atom 'b', got {:?}",
                    &updates[0].0
                );
                assert!(
                    matches!(&updates[0].1, CErlExpr::Lit(CErlLit::Int(99))),
                    "expected value Int 99, got {:?}",
                    &updates[0].1
                );
            }
            other => panic!("expected MapUpdate, got {other:?}"),
        }
    }

    // ── Peephole does NOT fire when all fields are fresh (no forwarding) ──────

    #[test]
    fn expr_construct_no_peephole_when_all_fresh() {
        // Construct { Record, fields: [("a", Int 1), ("b", Int 2)] }
        // No Field-projections at all → MapLit (peephole skipped).
        let expr = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: TyConId(0),
                name: "Point".into(),
                variant: 0,
            },
            fields: vec![("a".into(), lit_int(1)), ("b".into(), lit_int(2))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        assert!(
            matches!(result, CErlExpr::MapLit(_)),
            "expected MapLit (peephole should not fire), got {result:?}"
        );
    }

    // ── Lambda lowering helper ────────────────────────────────────────────────

    fn ir_param(name: &str) -> IrParam {
        IrParam {
            name: name.into(),
            ty: Type::Error,
            span: sp(),
        }
    }

    fn lambda(params: Vec<IrParam>, body: IrExpr) -> IrExpr {
        IrExpr::Lambda {
            id: node(),
            params,
            body: Box::new(body),
            caps: CapabilitySet::PURE,
            span: sp(),
        }
    }

    // ── Zero-param lambda → Fun { params: [], body } ─────────────────────────

    #[test]
    fn lambda_zero_params_emits_fun() {
        // Lambda { params: [], body: Lit Int 0 } → Fun { params: [], body: Int 0 }
        let expr = lambda(vec![], lit_int(0));
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Fun { params, body } => {
                assert!(params.is_empty(), "expected zero params");
                assert!(matches!(*body, CErlExpr::Lit(CErlLit::Int(0))));
            }
            other => panic!("expected Fun, got {other:?}"),
        }
    }

    // ── Multi-param lambda → Fun with mangled params ──────────────────────────

    #[test]
    fn lambda_multi_params_emits_fun_with_mangled_vars() {
        // Lambda { params: [x, y], body: Local "x" }
        // → Fun { params: [V_X, V_Y], body: Var V_X }
        let expr = lambda(vec![ir_param("x"), ir_param("y")], local("x"));
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Fun { params, body } => {
                assert_eq!(params.len(), 2);
                assert_eq!(params[0].0, "V_X");
                assert_eq!(params[1].0, "V_Y");
                assert!(matches!(*body, CErlExpr::Var(CErlVar(ref s)) if s == "V_X"));
            }
            other => panic!("expected Fun, got {other:?}"),
        }
    }

    // ── Lambda body references an outer local (capture) ──────────────────────

    #[test]
    fn lambda_body_captures_outer_local() {
        // LetIn "count" 42 (Lambda { params: [], body: Local "count" })
        // The lambda body sees "count" as a free variable → Var V_Count
        // (Erlang closures capture by name automatically; no closure-record).
        let inner = lambda(vec![], local("count"));
        let expr = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Bind {
                name: "count".into(),
                inner: None,
                span: sp(),
            },
            value: Box::new(lit_int(42)),
            body: Box::new(inner),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        // Outer: Let V_Count = 42 in Fun { params: [], body: Var V_Count }
        match result {
            CErlExpr::Let { body, .. } => match *body {
                CErlExpr::Fun {
                    params,
                    body: fun_body,
                } => {
                    assert!(params.is_empty());
                    // The lambda body is Var V_Count — the outer name is
                    // captured by name in the fresh lambda scope.
                    assert!(
                        matches!(*fun_body, CErlExpr::Var(CErlVar(ref s)) if s == "V_Count"),
                        "expected Var V_Count in lambda body, got {fun_body:?}"
                    );
                }
                other => panic!("expected Fun in let body, got {other:?}"),
            },
            other => panic!("expected Let, got {other:?}"),
        }
    }

    // ── Call with Constructor UnionVariant callee → Tuple ────────────────────

    #[test]
    fn call_union_variant_ctor_emits_tuple() {
        // Call { callee: Symbol(Constructor UnionVariant "Cons"), args: [Int 1, Int 2] }
        // → Tuple([Atom "Cons", Int 1, Int 2])
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::Constructor {
                    ctor_kind: CtorKind::UnionVariant,
                    owner_type: TyConId(0),
                    name: "Cons".into(),
                    variant: 0,
                },
                span: sp(),
            }),
            args: vec![lit_int(1), lit_int(2)],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 3);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "Cons")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(1))));
                assert!(matches!(&elems[2], CErlExpr::Lit(CErlLit::Int(2))));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    // ── Call Prelude "Some" with 1 arg → {some, A0} ──────────────────────────

    #[test]
    fn call_prelude_some_emits_some_tuple() {
        // Call { callee: Symbol(Prelude "Some"), args: [Int 7] }
        // → Tuple([Atom "some", Int 7])
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::Prelude {
                    name: "Some".into(),
                },
                span: sp(),
            }),
            args: vec![lit_int(7)],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2);
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "some")
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(7))));
            }
            other => panic!("expected Tuple([some, 7]), got {other:?}"),
        }
    }

    // ── Call Stdlib callee — bridge map dispatch ──────────────────────────────

    #[test]
    fn call_stdlib_unknown_module_emits_e002() {
        // A SymbolRef::Stdlib with a module name that has no bridge entry should
        // produce E002 StdlibBridgeMissing (not IrShapeMalformed).
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::Stdlib {
                    module: "std.unknown".into(),
                    name: "bogus".into(),
                },
                span: sp(),
            }),
            args: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr);
        assert!(
            matches!(result, Err(CodegenError::StdlibBridgeMissing { .. })),
            "expected StdlibBridgeMissing, got {result:?}"
        );
    }

    #[test]
    fn call_stdlib_known_emits_beam_call() {
        // Call { callee: Symbol(Stdlib "std.io"."println"), args: [lit "hello"] }
        // → Call { module: "ridge_rt", fn_name: "println", args: [Text "hello"] }
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::Stdlib {
                    module: "std.io".into(),
                    name: "println".into(),
                },
                span: sp(),
            }),
            args: vec![lit_text("hello")],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Call {
                module,
                fn_name,
                args,
            } => {
                assert_eq!(module.0, "ridge_rt");
                assert_eq!(fn_name.0, "println");
                assert_eq!(args.len(), 1);
            }
            other => panic!("expected CErlExpr::Call, got {other:?}"),
        }
    }

    // ── Call with dynamic callee (Lambda) → Apply ────────────────────────────

    #[test]
    fn call_dynamic_lambda_callee_emits_apply() {
        // Call { callee: Lambda { params: [x], body: Local "x" }, args: [Int 5] }
        // → Apply { callee: Fun { [V_X], Var V_X }, args: [Int 5] }
        let callee = lambda(vec![ir_param("x")], local("x"));
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(callee),
            args: vec![lit_int(5)],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Apply { callee, args } => {
                assert!(
                    matches!(*callee, CErlExpr::Fun { .. }),
                    "expected Fun callee, got {callee:?}"
                );
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], CErlExpr::Lit(CErlLit::Int(5))));
            }
            other => panic!("expected Apply, got {other:?}"),
        }
    }

    // ── LetRec end-to-end — recursive inner fn ───────────────────────────────

    #[test]
    fn letin_recursive_lambda_emits_letrec() {
        // LetIn { pat: Bind("loop", None),
        //   value: Lambda { params: [], body: Call { callee: Local "loop", args: [] } },
        //   body: Call { callee: Local "loop", args: [] }
        // }
        // The lambda body references "loop" → letrec.
        let self_call = IrExpr::Call {
            id: node(),
            callee: Box::new(local("loop")),
            args: vec![],
            span: sp(),
        };
        let rec_lambda = lambda(vec![], self_call.clone());
        let expr = IrExpr::LetIn {
            id: node(),
            pat: IrPat::Bind {
                name: "loop".into(),
                inner: None,
                span: sp(),
            },
            value: Box::new(rec_lambda),
            body: Box::new(self_call),
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::LetRec { defs, body } => {
                assert_eq!(defs.len(), 1, "expected one letrec def");
                // defs[0] is now (CErlAtom name, u32 arity, CErlExpr fun_expr).
                assert_eq!(defs[0].0 .0, "loop", "letrec atom name should be 'loop'");
                assert_eq!(defs[0].1, 0, "letrec arity should be 0");
                assert!(
                    matches!(defs[0].2, CErlExpr::Fun { .. }),
                    "letrec def value should be Fun, got {:?}",
                    defs[0].2
                );
                // body is Call { callee: Local "loop", args: [] }
                // → Apply { callee: LocalFnRef("loop", 0), args: [] }
                // (because "loop" is in fn_arity during body lowering)
                assert!(
                    matches!(*body, CErlExpr::Apply { .. }),
                    "letrec body should be Apply, got {body:?}"
                );
            }
            other => panic!("expected LetRec, got {other:?}"),
        }
    }

    // ── Call Local callee → apply 'fnName'/arity (args...) ──────────────────

    #[test]
    fn call_local_callee_emits_apply_local_fn_ref() {
        // Call { callee: Symbol(Local "my_fn"), args: [] } → Apply { LocalFnRef("my_fn"/0), [] }
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::Local {
                    name: "my_fn".into(),
                    module: ridge_ir::ModuleId(0),
                },
                span: sp(),
            }),
            args: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Ok(CErlExpr::Apply { callee, args }) => {
                assert!(
                    matches!(*callee, CErlExpr::LocalFnRef { ref name, arity: 0 } if name.0 == "my_fn"),
                    "expected LocalFnRef(my_fn/0), got {callee:?}"
                );
                assert!(args.is_empty(), "expected 0 args, got {args:?}");
            }
            other => panic!("expected Apply {{ LocalFnRef }}, got {other:?}"),
        }
    }

    // ── Call Handler callee → defensive IrShapeMalformed ─────────────────────

    #[test]
    fn call_handler_callee_returns_defensive_error() {
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::Handler {
                    actor: "MyActor".into(),
                    handler: "on_msg".into(),
                    actor_module: ridge_ir::ModuleId(0),
                },
                span: sp(),
            }),
            args: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr);
        match result {
            Err(CodegenError::IrShapeMalformed {
                variant, detail, ..
            }) => {
                assert_eq!(variant, "IrExpr::Call");
                assert!(
                    detail.contains("Handler") && detail.contains("Phase 5 invariant violated"),
                    "unexpected detail: {detail}"
                );
            }
            other => panic!("expected IrShapeMalformed, got {other:?}"),
        }
    }

    // ── Cross-module call arity (unit-paren shim over External callees) ───────

    #[test]
    fn call_external_zero_arity_drops_the_unit_paren() {
        // `f ()` to a zero-arity fn in another module carries a single Unit arg
        // in the IR, but the callee compiles to arity 0 in its own module — so
        // the cross-module call must drop the `()` and emit an arity-0 qualified
        // call, not an arity-1 call that would be `undef` against `f/0`.
        let callee = ridge_ir::ModuleId(3);
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::External {
                    module: callee,
                    name: "getVal".into(),
                },
                span: sp(),
            }),
            args: vec![IrExpr::Lit {
                id: node(),
                value: IrLit::Unit,
                span: sp(),
            }],
            span: sp(),
        };
        let mut scope = LocalScope::new();
        let mut names = rustc_hash::FxHashMap::default();
        names.insert("getVal".to_owned(), 0u32);
        let mut table = rustc_hash::FxHashMap::default();
        table.insert(callee, names);
        scope.external_arity = std::sync::Arc::new(table);
        match lower_expr_in_scope(&expr, &mut scope).expect("lowers") {
            CErlExpr::Call { fn_name, args, .. } => {
                assert_eq!(fn_name.0, "getVal");
                assert!(
                    args.is_empty(),
                    "a 0-arity cross-module call must drop the unit paren, got {args:?}"
                );
            }
            other => panic!("expected a qualified Call, got {other:?}"),
        }
    }

    #[test]
    fn call_external_unit_param_keeps_the_arg() {
        // A cross-module callee that genuinely takes a Unit is arity 1 — the
        // shim must NOT strip the argument.
        let callee = ridge_ir::ModuleId(3);
        let expr = IrExpr::Call {
            id: node(),
            callee: Box::new(IrExpr::Symbol {
                id: node(),
                sym: SymbolRef::External {
                    module: callee,
                    name: "sink".into(),
                },
                span: sp(),
            }),
            args: vec![IrExpr::Lit {
                id: node(),
                value: IrLit::Unit,
                span: sp(),
            }],
            span: sp(),
        };
        let mut scope = LocalScope::new();
        let mut names = rustc_hash::FxHashMap::default();
        names.insert("sink".to_owned(), 1u32);
        let mut table = rustc_hash::FxHashMap::default();
        table.insert(callee, names);
        scope.external_arity = std::sync::Arc::new(table);
        match lower_expr_in_scope(&expr, &mut scope).expect("lowers") {
            CErlExpr::Call { args, .. } => {
                assert_eq!(args.len(), 1, "an arity-1 external call keeps its Unit arg");
            }
            other => panic!("expected a qualified Call, got {other:?}"),
        }
    }

    // ── Guard-lift helpers ───────────────────────────────────────────────────

    #[test]
    fn contains_non_bif_call_passes_erlang_calls() {
        let expr = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("=:=".into()),
            args: vec![
                CErlExpr::Var(CErlVar("X".into())),
                CErlExpr::Lit(CErlLit::Int(0)),
            ],
        };
        assert!(!contains_non_bif_call(&expr));
    }

    #[test]
    fn contains_non_bif_call_flags_stdlib_calls() {
        let expr = CErlExpr::Call {
            module: CErlAtom("std.int".into()),
            fn_name: CErlAtom("mod".into()),
            args: vec![
                CErlExpr::Var(CErlVar("X".into())),
                CErlExpr::Lit(CErlLit::Int(15)),
            ],
        };
        assert!(contains_non_bif_call(&expr));
    }

    #[test]
    fn contains_non_bif_call_finds_nested_stdlib_calls() {
        // `erlang:'=:='('std.int':mod(X, 15), 0)` — outer is a guard BIF, the
        // inner stdlib call still disqualifies the whole expression.
        let inner = CErlExpr::Call {
            module: CErlAtom("std.int".into()),
            fn_name: CErlAtom("mod".into()),
            args: vec![
                CErlExpr::Var(CErlVar("X".into())),
                CErlExpr::Lit(CErlLit::Int(15)),
            ],
        };
        let outer = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("=:=".into()),
            args: vec![inner, CErlExpr::Lit(CErlLit::Int(0))],
        };
        assert!(contains_non_bif_call(&outer));
    }

    #[test]
    fn contains_non_bif_call_flags_apply() {
        // `apply Fn (X)` — calling through a fun reference is never legal
        // in a clause guard.
        let expr = CErlExpr::Apply {
            callee: Box::new(CErlExpr::Var(CErlVar("F".into()))),
            args: vec![CErlExpr::Var(CErlVar("X".into()))],
        };
        assert!(contains_non_bif_call(&expr));
    }

    /// Regression: not every `erlang:*` function is a guard BIF.
    /// `erlang:integer_to_binary/1` is reachable via `@ffi("erlang",
    /// "integer_to_binary", 1)` in stdlib but is NOT permitted in clause
    /// guards. Without the per-name whitelist, this call slipped past the
    /// loose `m == "erlang"` check and BEAM rejected the resulting code at
    /// load time. The guard must now be lifted instead.
    #[test]
    fn contains_non_bif_call_flags_non_guard_erlang_fn() {
        let expr = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("integer_to_binary".into()),
            args: vec![CErlExpr::Var(CErlVar("N".into()))],
        };
        assert!(contains_non_bif_call(&expr));
    }

    /// Regression: a comparison wrapped around a non-guard `erlang:*` call
    /// must propagate the inner unsafety even though the outer `=:=` is a
    /// guard BIF. This is the realistic shape: `Int.toText n == "0"` lowers
    /// to `=:= (integer_to_binary n) "0"`.
    #[test]
    fn contains_non_bif_call_flags_non_guard_erlang_inside_eq() {
        let inner = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("integer_to_binary".into()),
            args: vec![CErlExpr::Var(CErlVar("N".into()))],
        };
        let outer = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("=:=".into()),
            args: vec![inner, CErlExpr::Lit(CErlLit::Int(0))],
        };
        assert!(contains_non_bif_call(&outer));
    }

    /// Sanity: explicit arity matters. `erlang:abs/1` IS a guard BIF; an
    /// `erlang:abs` call with the wrong number of args (which can't actually
    /// occur from legitimate codegen) would not be on the whitelist.
    #[test]
    fn contains_non_bif_call_respects_arity() {
        let abs_1 = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("abs".into()),
            args: vec![CErlExpr::Lit(CErlLit::Int(-3))],
        };
        assert!(!contains_non_bif_call(&abs_1));

        let abs_2 = CErlExpr::Call {
            module: CErlAtom("erlang".into()),
            fn_name: CErlAtom("abs".into()),
            args: vec![
                CErlExpr::Lit(CErlLit::Int(-3)),
                CErlExpr::Lit(CErlLit::Int(0)),
            ],
        };
        assert!(contains_non_bif_call(&abs_2));
    }

    #[test]
    #[allow(clippy::cognitive_complexity)] // a single end-to-end scenario test
    fn lift_guarded_match_wraps_unsafe_guard_in_inner_case() {
        // Mirrors a 2-arm match where arm 0's guard calls `std.int:mod` and
        // arm 1 is an unguarded wildcard fallback. Expected shape with the
        // continuation-thunk refactor:
        //   let V_LiftedRest0 = fun () -> <rest> end in
        //       case V_S of
        //           V_M -> case <unsafe-guard> of
        //                      'true' -> 1
        //                      _      -> apply V_LiftedRest0 ()
        //                  end
        //           _   -> apply V_LiftedRest0 ()
        //       end
        let scrut_var = CErlVar("V_S".into());
        let unsafe_guard = CErlExpr::Call {
            module: CErlAtom("std.int".into()),
            fn_name: CErlAtom("mod".into()),
            args: vec![
                CErlExpr::Var(CErlVar("V_M".into())),
                CErlExpr::Lit(CErlLit::Int(15)),
            ],
        };
        let arms = vec![
            LoweredArm {
                pattern: crate::core_ast::CErlPat::Var(CErlVar("V_M".into())),
                guard: unsafe_guard,
                body: CErlExpr::Lit(CErlLit::Int(1)),
                guard_is_safe: false,
            },
            LoweredArm {
                pattern: crate::core_ast::CErlPat::Wild,
                guard: lit_true(),
                body: CErlExpr::Lit(CErlLit::Int(0)),
                guard_is_safe: true,
            },
        ];
        let result = lift_guarded_match(&scrut_var, &arms);

        // Outer shape is `let V_LiftedRest0 = fun () -> <rest> end in <case>`.
        let (rest_fn_body, case_expr) = match result {
            CErlExpr::Let { var, value, body } => {
                assert_eq!(var.0, "V_LiftedRest0", "thunk var must be V_LiftedRest0");
                let fn_body = match *value {
                    CErlExpr::Fun { params, body } => {
                        assert!(
                            params.is_empty(),
                            "rest thunk must be 0-arity, got params {params:?}"
                        );
                        *body
                    }
                    other => panic!("expected rest binding to be a Fun, got {other:?}"),
                };
                (fn_body, *body)
            }
            other => panic!("expected outer Let, got {other:?}"),
        };

        // The thunk body is the remaining arms — here a single wildcard ->
        // 0 — re-emitted as a Case. Just sanity-check it is a Case so we
        // know rest hoisting happened.
        assert!(
            matches!(rest_fn_body, CErlExpr::Case { .. }),
            "rest thunk body must wrap the remaining arms in a Case, got {rest_fn_body:?}"
        );

        let clauses = match case_expr {
            CErlExpr::Case { scrutinee, clauses } => {
                assert!(matches!(*scrutinee, CErlExpr::Var(CErlVar(ref s)) if s == "V_S"));
                clauses
            }
            other => panic!("expected inner Case, got {other:?}"),
        };
        assert_eq!(
            clauses.len(),
            2,
            "expected lifted arm + wildcard catch-all, got {} clauses",
            clauses.len()
        );

        // First clause: V_M -> case Guard of 'true' -> 1 ; _ -> apply V_LiftedRest0 () end
        assert!(matches!(
            &clauses[0].pattern,
            crate::core_ast::CErlPat::Var(_)
        ));
        match &clauses[0].body {
            CErlExpr::Case {
                clauses: inner_clauses,
                ..
            } => {
                assert_eq!(inner_clauses.len(), 2);
                assert!(matches!(
                    &inner_clauses[0].pattern,
                    crate::core_ast::CErlPat::Lit(CErlLit::Atom(CErlAtom(s))) if s == "true"
                ));
                assert!(matches!(
                    inner_clauses[0].body,
                    CErlExpr::Lit(CErlLit::Int(1))
                ));
                assert!(matches!(
                    &inner_clauses[1].pattern,
                    crate::core_ast::CErlPat::Wild
                ));
                // Inner fall-through must invoke the rest thunk, not duplicate
                // the rest expression inline.
                match &inner_clauses[1].body {
                    CErlExpr::Apply { callee, args } => {
                        assert!(args.is_empty());
                        assert!(
                            matches!(callee.as_ref(), CErlExpr::Var(CErlVar(s)) if s == "V_LiftedRest0"),
                            "guard fall-through must apply V_LiftedRest0, got {callee:?}"
                        );
                    }
                    other => panic!("expected Apply V_LiftedRest0 (), got {other:?}"),
                }
            }
            other => panic!("expected inner Case on the guard, got {other:?}"),
        }

        // Second clause: _ -> apply V_LiftedRest0 () (the rest is hoisted, not
        // duplicated).
        assert!(matches!(
            &clauses[1].pattern,
            crate::core_ast::CErlPat::Wild
        ));
        match &clauses[1].body {
            CErlExpr::Apply { callee, args } => {
                assert!(args.is_empty());
                assert!(
                    matches!(callee.as_ref(), CErlExpr::Var(CErlVar(s)) if s == "V_LiftedRest0"),
                    "outer wildcard must apply V_LiftedRest0, got {callee:?}"
                );
            }
            other => panic!("expected outer wildcard body to apply V_LiftedRest0, got {other:?}"),
        }
    }

    #[test]
    fn match_with_safe_guards_takes_fast_path() {
        // `match x { m when m < 0 -> 1 ; _ -> 0 }`
        // The guard is `erlang:'<'` (a guard BIF), so the lowerer must emit
        // a plain `case Scrut of P when G -> B end` — no enclosing `Let`.
        let expr = IrExpr::Match {
            id: node(),
            scrutinee: Box::new(local("x")),
            arms: vec![
                IrArm {
                    pat: IrPat::Bind {
                        name: "m".into(),
                        inner: None,
                        span: sp(),
                    },
                    when: Some(IrExpr::Call {
                        id: node(),
                        callee: Box::new(IrExpr::Symbol {
                            id: node(),
                            sym: SymbolRef::Stdlib {
                                module: "std.op".into(),
                                name: "lt".into(),
                            },
                            span: sp(),
                        }),
                        args: vec![local("m"), lit_int(0)],
                        span: sp(),
                    }),
                    body: lit_int(1),
                    span: sp(),
                },
                IrArm {
                    pat: IrPat::Wild { span: sp() },
                    when: None,
                    body: lit_int(0),
                    span: sp(),
                },
            ],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        assert!(
            matches!(result, CErlExpr::Case { .. }),
            "expected plain Case (fast path), got {result:?}"
        );
    }

    // ── Union-variant Construct → tagged tuple ───────────────────────────────
    //
    // Integration test: verifies that an `IrExpr::Construct` with
    // `CtorKind::UnionVariant` and positional fields emits the correct
    // Core Erlang tagged-tuple `{Name, v1, v2, …}`.

    fn union_ctor(name: &str) -> SymbolRef {
        SymbolRef::Constructor {
            ctor_kind: CtorKind::UnionVariant,
            owner_type: TyConId(0),
            name: name.into(),
            variant: 1,
        }
    }

    /// `Circle 5` folded to `IrExpr::Construct { UnionVariant("Circle"), [(_, Int 5)] }`
    /// must emit `{circle, 5}` — a two-element Core Erlang tuple.
    #[test]
    fn construct_union_variant_one_field_emits_tagged_tuple() {
        let expr = IrExpr::Construct {
            id: node(),
            ctor: union_ctor("Circle"),
            fields: vec![(String::new(), lit_int(5))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 2, "expected tag + 1 value");
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "Circle"),
                    "first element must be atom 'Circle'"
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(5))));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    /// `Rectangle 4 6` folded to `IrExpr::Construct` with two fields must
    /// emit `{rectangle, 4, 6}` — a three-element Core Erlang tuple.
    #[test]
    fn construct_union_variant_two_fields_emits_tagged_tuple() {
        let expr = IrExpr::Construct {
            id: node(),
            ctor: union_ctor("Rectangle"),
            fields: vec![(String::new(), lit_int(4)), (String::new(), lit_int(6))],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        match result {
            CErlExpr::Tuple(elems) => {
                assert_eq!(elems.len(), 3, "expected tag + 2 values");
                assert!(
                    matches!(&elems[0], CErlExpr::Lit(CErlLit::Atom(CErlAtom(s))) if s == "Rectangle"),
                    "first element must be atom 'Rectangle'"
                );
                assert!(matches!(&elems[1], CErlExpr::Lit(CErlLit::Int(4))));
                assert!(matches!(&elems[2], CErlExpr::Lit(CErlLit::Int(6))));
            }
            other => panic!("expected Tuple, got {other:?}"),
        }
    }

    /// Nullary `IrExpr::Construct { UnionVariant("Red"), fields: [] }` must
    /// emit a bare atom `'Red'`, not a tuple — regression guard.
    #[test]
    fn construct_union_variant_zero_fields_emits_bare_atom() {
        let expr = IrExpr::Construct {
            id: node(),
            ctor: union_ctor("Red"),
            fields: vec![],
            span: sp(),
        };
        let result = lower_expr(&expr).unwrap();
        assert!(
            matches!(result, CErlExpr::Lit(CErlLit::Atom(CErlAtom(ref s))) if s == "Red"),
            "nullary variant must emit bare atom 'Red', got {result:?}"
        );
    }

    // ── Static-dict peephole ──────────────────────────────────────────────────

    /// `IrExpr::Field { base: Construct(Record, [(K, V)]), field: K }` folds to
    /// `V` directly — no `maps:get` call emitted (static-dict peephole).
    ///
    /// This exercises the typeclass dictionary lowering path: when the dict arg
    /// is a literal [`IrExpr::Construct`] (Record kind), `lower_field` detects
    /// the key statically and skips the runtime map lookup.
    #[test]
    fn field_on_literal_record_construct_peephole_fires() {
        use ridge_ir::{CtorKind, SymbolRef};
        use ridge_types::TyConId;

        // Build a literal dict: `#{ 'toText' => 42 }` (an int stands in for a fun ref).
        let method_value = IrExpr::Lit {
            id: node(),
            value: IrLit::Int(42),
            span: sp(),
        };
        let dict = IrExpr::Construct {
            id: node(),
            ctor: SymbolRef::Constructor {
                ctor_kind: CtorKind::Record,
                owner_type: TyConId(0),
                name: "$inst_Show_Color".into(),
                variant: 0,
            },
            fields: vec![("toText".into(), method_value)],
            span: sp(),
        };

        // Projection: `dict.toText` — should fold to the value directly.
        let field_expr = IrExpr::Field {
            id: node(),
            base: Box::new(dict),
            field: "toText".into(),
            span: sp(),
        };

        let result = lower_expr(&field_expr).unwrap();

        // The peephole must have fired: result is the Int(42), NOT a maps:get call.
        assert!(
            matches!(result, CErlExpr::Lit(CErlLit::Int(42))),
            "static-dict peephole must fold to the value directly, got {result:?}"
        );
    }

    /// `IrExpr::Field` on a non-literal base (e.g. a `Local` variable) must NOT
    /// fire the peephole — it emits the standard `maps:get` call.
    #[test]
    fn field_on_local_variable_does_not_peephole() {
        let dict_var = IrExpr::Local {
            id: node(),
            name: "$dict_Show_0".into(),
            span: sp(),
        };
        let field_expr = IrExpr::Field {
            id: node(),
            base: Box::new(dict_var),
            field: "toText".into(),
            span: sp(),
        };

        let result = lower_expr(&field_expr).unwrap();

        // Must be a `maps:get` call — peephole must NOT fire for non-literals.
        assert!(
            matches!(
                &result,
                CErlExpr::Call { module, fn_name, .. }
                    if module.0 == "maps" && fn_name.0 == "get"
            ),
            "non-literal base must emit maps:get, got {result:?}"
        );
    }
}
