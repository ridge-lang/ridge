//! ANF (Administrative Normal Form) normalisation pass for Core Erlang.
//!
//! Core Erlang requires that all `call`/`apply`/`case scrutinee` arguments be
//! **atomic**: variables, atoms, integers, floats, or binary literals.  Compound
//! expressions (other `call`s, tuples, map literals, etc.) are not permitted in
//! argument position — `erlc` rejects them with "illegal expression".
//!
//! This pass hoists non-atomic arguments into fresh `let`-bindings, converting:
//! ```text
//! call 'f'(call 'g'(x), call 'h'(y))
//! ```
//! into:
//! ```text
//! let <V_anf_0> = call 'g'(x) in
//!   let <V_anf_1> = call 'h'(y) in
//!     call 'f'(V_anf_0, V_anf_1)
//! ```
//!
//! The same hoisting applies to `apply` callees and arguments, map literal
//! values and keys, tuple elements, cons cells, map update base/values, and
//! `case` scrutinees.
//!
//! ## Atomicity definition (Core Erlang §2.1)
//!
//! The following `CErlExpr` shapes are atomic (safe in argument position):
//! - `Lit(_)` — any literal (integer, float, atom, binary, nil, empty tuple)
//! - `Var(_)` — a variable
//! - `LocalFnRef { .. }` — a local function reference `'name'/arity`
//!
//! Everything else is compound and must be hoisted.
//!
//! ## Counter stability
//!
//! The ANF counter is thread-local and deterministic per expression tree
//! (it counts from 0 per `normalise_module` call, not globally).  This keeps
//! snapshot diffs stable across re-runs.
//!
//! ## Recursion contract
//!
//! The pass descends into every sub-expression:
//! - `Fun` body: recurse fully (may contain nested calls)
//! - `Let` value + body: recurse
//! - `LetRec` fun bodies + outer body: recurse
//! - `Case` scrutinee (hoist if non-atomic) + all clause bodies + guards
//! - `Do` first + then: recurse
//! - `Tuple` elements: recurse (hoist non-atomic)
//! - `Cons` head/tail: recurse (hoist non-atomic)
//! - `ListLit` elements: recurse (hoist non-atomic)
//! - `MapLit` keys + values: recurse (hoist non-atomic)
//! - `MapUpdate` base + values: recurse (hoist non-atomic)
//! - `Call` args: recurse (hoist non-atomic)
//! - `Apply` callee + args: recurse (hoist non-atomic)
//! - `Try` body + clause bodies: recurse

// pub(crate) items are used from lib.rs / module.rs.
#![allow(clippy::redundant_pub_crate)]
// CErlExpr is #[non_exhaustive]; the catch-all arm for future variants is
// unreachable with the current variant set but required for forward safety.
#![allow(unreachable_patterns)]

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlFn, CErlModule, CErlVar};

// ── Atomicity predicate ───────────────────────────────────────────────────────

/// Returns `true` iff `expr` is safe to use in argument position (atomic).
const fn is_atomic(expr: &CErlExpr) -> bool {
    matches!(
        expr,
        CErlExpr::Var(_) | CErlExpr::Lit(_) | CErlExpr::LocalFnRef { .. }
    )
}

// ── ANF counter ───────────────────────────────────────────────────────────────

/// Mutable counter threaded through the normalisation pass.
struct Anf {
    next: u32,
}

impl Anf {
    const fn new() -> Self {
        Self { next: 0 }
    }

    /// Allocate the next synthetic ANF variable name.
    fn fresh(&mut self) -> CErlVar {
        let n = self.next;
        self.next += 1;
        CErlVar(format!("V_Anf{n}"))
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Normalise all function bodies in a `CErlModule` to ANF.
///
/// Called by `module::lower_module_all` after codegen and before printing.
pub(crate) fn normalise_module(module: &mut CErlModule) {
    let mut anf = Anf::new();
    for f in &mut module.fns {
        normalise_fn(f, &mut anf);
    }
}

/// Normalise a single top-level function.
fn normalise_fn(f: &mut CErlFn, anf: &mut Anf) {
    let body = std::mem::replace(
        &mut f.body,
        CErlExpr::Lit(crate::core_ast::CErlLit::Atom(CErlAtom("ok".into()))),
    );
    f.body = normalise_expr(body, anf);
}

// ── Expression normalisation ──────────────────────────────────────────────────

/// Normalise an expression recursively, returning the normalised form.
///
/// This function never returns a wrapper `let` at the top level — wrapping is
/// done by the callers (via `hoist_if_needed`).
#[allow(clippy::too_many_lines)]
// reason: cohesive lowering pass, splitting hurts readability
fn normalise_expr(expr: CErlExpr, anf: &mut Anf) -> CErlExpr {
    match expr {
        // Fun: normalise the body.
        CErlExpr::Fun { params, body } => CErlExpr::Fun {
            params,
            body: Box::new(normalise_expr(*body, anf)),
        },

        // Call: normalise args; hoist non-atomics.
        CErlExpr::Call {
            module,
            fn_name,
            args,
        } => {
            let (bindings, normal_args) = hoist_args(args, anf);
            let call = CErlExpr::Call {
                module,
                fn_name,
                args: normal_args,
            };
            wrap_bindings(bindings.into_iter(), call)
        }

        // Apply: normalise callee + args; hoist non-atomics.
        CErlExpr::Apply { callee, args } => {
            // Normalise callee sub-expression first.
            let normal_callee = normalise_expr(*callee, anf);
            // Hoist non-atomic callee.
            let (callee_expr, mut outer_bindings) = hoist_single(normal_callee, anf);
            // Hoist args.
            let (mut arg_bindings, normal_args) = hoist_args(args, anf);
            outer_bindings.append(&mut arg_bindings);
            let apply = CErlExpr::Apply {
                callee: Box::new(callee_expr),
                args: normal_args,
            };
            wrap_bindings(outer_bindings.into_iter(), apply)
        }

        // Let: normalise value + body (no hoisting needed at let level).
        CErlExpr::Let { var, value, body } => CErlExpr::Let {
            var,
            value: Box::new(normalise_expr(*value, anf)),
            body: Box::new(normalise_expr(*body, anf)),
        },

        // LetRec: normalise each fun body + the outer body.
        CErlExpr::LetRec { defs, body } => {
            let normal_defs = defs
                .into_iter()
                .map(|(name, arity, fun_expr)| (name, arity, normalise_expr(fun_expr, anf)))
                .collect();
            CErlExpr::LetRec {
                defs: normal_defs,
                body: Box::new(normalise_expr(*body, anf)),
            }
        }

        // Case: hoist non-atomic scrutinee; normalise clause guards + bodies.
        CErlExpr::Case { scrutinee, clauses } => {
            let normal_scrutinee = normalise_expr(*scrutinee, anf);
            let (scrutinee_expr, bindings) = hoist_single(normal_scrutinee, anf);
            let normal_clauses = clauses
                .into_iter()
                .map(|c| normalise_clause(c, anf))
                .collect();
            let case = CErlExpr::Case {
                scrutinee: Box::new(scrutinee_expr),
                clauses: normal_clauses,
            };
            wrap_bindings(bindings.into_iter(), case)
        }

        // Do: normalise both branches.
        CErlExpr::Do { first, then } => CErlExpr::Do {
            first: Box::new(normalise_expr(*first, anf)),
            then: Box::new(normalise_expr(*then, anf)),
        },

        // Tuple: hoist non-atomic elements.
        CErlExpr::Tuple(elems) => {
            let (bindings, normal_elems) = hoist_args(elems, anf);
            let tup = CErlExpr::Tuple(normal_elems);
            wrap_bindings(bindings.into_iter(), tup)
        }

        // Cons: hoist non-atomic head/tail.
        CErlExpr::Cons { head, tail } => {
            let normal_head = normalise_expr(*head, anf);
            let normal_tail = normalise_expr(*tail, anf);
            let (head_expr, mut h_bindings) = hoist_single(normal_head, anf);
            let (tail_expr, mut t_bindings) = hoist_single(normal_tail, anf);
            h_bindings.append(&mut t_bindings);
            let cons = CErlExpr::Cons {
                head: Box::new(head_expr),
                tail: Box::new(tail_expr),
            };
            wrap_bindings(h_bindings.into_iter(), cons)
        }

        // ListLit: hoist non-atomic elements.
        CErlExpr::ListLit(elems) => {
            let (bindings, normal_elems) = hoist_args(elems, anf);
            let list = CErlExpr::ListLit(normal_elems);
            wrap_bindings(bindings.into_iter(), list)
        }

        // MapLit: hoist non-atomic keys and values.
        CErlExpr::MapLit(pairs) => {
            let mut all_bindings: Vec<(CErlVar, CErlExpr)> = Vec::new();
            let mut normal_pairs: Vec<(CErlExpr, CErlExpr)> = Vec::new();
            for (k, v) in pairs {
                let nk = normalise_expr(k, anf);
                let nv = normalise_expr(v, anf);
                let (ka, mut kb) = hoist_single(nk, anf);
                let (va, mut vb) = hoist_single(nv, anf);
                all_bindings.append(&mut kb);
                all_bindings.append(&mut vb);
                normal_pairs.push((ka, va));
            }
            let map = CErlExpr::MapLit(normal_pairs);
            wrap_bindings(all_bindings.into_iter(), map)
        }

        // MapUpdate: hoist non-atomic base and update values.
        CErlExpr::MapUpdate { base, updates } => {
            let normal_base = normalise_expr(*base, anf);
            let (base_expr, mut all_bindings) = hoist_single(normal_base, anf);
            let mut normal_updates: Vec<(CErlExpr, CErlExpr)> = Vec::new();
            for (k, v) in updates {
                let nk = normalise_expr(k, anf);
                let nv = normalise_expr(v, anf);
                let (ka, mut kb) = hoist_single(nk, anf);
                let (va, mut vb) = hoist_single(nv, anf);
                all_bindings.append(&mut kb);
                all_bindings.append(&mut vb);
                normal_updates.push((ka, va));
            }
            let upd = CErlExpr::MapUpdate {
                base: Box::new(base_expr),
                updates: normal_updates,
            };
            wrap_bindings(all_bindings.into_iter(), upd)
        }

        // Receive: normalise clause bodies + after.
        CErlExpr::Receive { clauses, after } => {
            let normal_clauses = clauses
                .into_iter()
                .map(|c| normalise_clause(c, anf))
                .collect();
            let normal_after = after.map(|(timeout, body)| {
                (
                    Box::new(normalise_expr(*timeout, anf)),
                    Box::new(normalise_expr(*body, anf)),
                )
            });
            CErlExpr::Receive {
                clauses: normal_clauses,
                after: normal_after,
            }
        }

        // Try: normalise body + clause bodies.
        CErlExpr::Try { body, of, catch } => CErlExpr::Try {
            body: Box::new(normalise_expr(*body, anf)),
            of: of.into_iter().map(|c| normalise_clause(c, anf)).collect(),
            catch: catch
                .into_iter()
                .map(|c| normalise_clause(c, anf))
                .collect(),
        },

        // Catch-all for #[non_exhaustive]: pass through unchanged.
        _ => expr,
    }
}

/// Normalise a `CErlClause` (pattern is unchanged; guard + body are normalised).
fn normalise_clause(clause: CErlClause, anf: &mut Anf) -> CErlClause {
    CErlClause {
        pattern: clause.pattern,
        guard: normalise_expr(clause.guard, anf),
        body: normalise_expr(clause.body, anf),
    }
}

// ── Hoisting helpers ──────────────────────────────────────────────────────────

/// Normalise a list of argument expressions, hoisting any non-atomic results.
///
/// Returns `(bindings, atomic_exprs)`.  The bindings must be wrapped around
/// the surrounding call/apply expression.
fn hoist_args(args: Vec<CErlExpr>, anf: &mut Anf) -> (Vec<(CErlVar, CErlExpr)>, Vec<CErlExpr>) {
    let mut bindings: Vec<(CErlVar, CErlExpr)> = Vec::new();
    let mut normal_args: Vec<CErlExpr> = Vec::with_capacity(args.len());
    for arg in args {
        let normal = normalise_expr(arg, anf);
        let (atomic, mut arg_bindings) = hoist_single(normal, anf);
        bindings.append(&mut arg_bindings);
        normal_args.push(atomic);
    }
    (bindings, normal_args)
}

/// If `expr` is non-atomic, allocate a fresh binding `let V_AnfN = expr in` and
/// return `(V_AnfN, [(V_AnfN, expr)])`.  If already atomic, return `(expr, [])`.
fn hoist_single(expr: CErlExpr, anf: &mut Anf) -> (CErlExpr, Vec<(CErlVar, CErlExpr)>) {
    if is_atomic(&expr) {
        (expr, Vec::new())
    } else {
        let var = anf.fresh();
        let bindings = vec![(var.clone(), expr)];
        (CErlExpr::Var(var), bindings)
    }
}

/// Wrap an expression in a sequence of `let V = val in ...` bindings.
///
/// The bindings are applied outermost-first (left-to-right textual order).
/// If `bindings` is empty, returns `inner` unchanged.
fn wrap_bindings<I>(bindings: I, inner: CErlExpr) -> CErlExpr
where
    I: DoubleEndedIterator<Item = (CErlVar, CErlExpr)>,
{
    // Build right-to-left: innermost binding wraps innermost expression.
    let mut result = inner;
    for (var, value) in bindings.rev() {
        result = CErlExpr::Let {
            var,
            value: Box::new(value),
            body: Box::new(result),
        };
    }
    result
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core_ast::{CErlAtom, CErlExpr, CErlLit, CErlVar};

    fn atom(s: &str) -> CErlExpr {
        CErlExpr::Lit(CErlLit::Atom(CErlAtom(s.into())))
    }

    fn var(s: &str) -> CErlExpr {
        CErlExpr::Var(CErlVar(s.into()))
    }

    fn call(module: &str, fn_name: &str, args: Vec<CErlExpr>) -> CErlExpr {
        CErlExpr::Call {
            module: CErlAtom(module.into()),
            fn_name: CErlAtom(fn_name.into()),
            args,
        }
    }

    fn local_fn_ref(name: &str, arity: u32) -> CErlExpr {
        CErlExpr::LocalFnRef {
            name: CErlAtom(name.into()),
            arity,
        }
    }

    // Helper: collect binding vars + final call.
    fn collect_lets(mut expr: &CErlExpr) -> (Vec<String>, &CErlExpr) {
        let mut vars = Vec::new();
        loop {
            match expr {
                CErlExpr::Let { var, body, .. } => {
                    vars.push(var.0.clone());
                    expr = body;
                }
                other => return (vars, other),
            }
        }
    }

    #[test]
    fn atomic_arg_not_hoisted() {
        // call 'f'(V_X) — V_X is atomic, no hoisting.
        let expr = call("f", "fn", vec![var("V_X")]);
        let mut anf = Anf::new();
        let result = normalise_expr(expr, &mut anf);
        // Must remain a plain Call (no Let wrapper).
        assert!(matches!(result, CErlExpr::Call { .. }));
    }

    #[test]
    fn nested_call_arg_hoisted() {
        // call 'f'(call 'g'(V_X)) → let V_Anf0 = call 'g'(V_X) in call 'f'(V_Anf0)
        let inner = call("g", "fn", vec![var("V_X")]);
        let outer = call("f", "fn", vec![inner]);

        let mut anf = Anf::new();
        let result = normalise_expr(outer, &mut anf);

        let (let_vars, final_call) = collect_lets(&result);
        assert_eq!(let_vars.len(), 1, "one binding should be introduced");
        assert_eq!(let_vars[0], "V_Anf0");
        match final_call {
            CErlExpr::Call { args, .. } => {
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], CErlExpr::Var(CErlVar(s)) if s == "V_Anf0"));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn two_nested_calls_hoisted_in_order() {
        // call 'f'(call 'g'(V_X), call 'h'(V_Y))
        // → let V_Anf0 = call 'g'(V_X) in
        //     let V_Anf1 = call 'h'(V_Y) in
        //       call 'f'(V_Anf0, V_Anf1)
        let g = call("g", "fn", vec![var("V_X")]);
        let h = call("h", "fn", vec![var("V_Y")]);
        let f = call("f", "fn", vec![g, h]);

        let mut anf = Anf::new();
        let result = normalise_expr(f, &mut anf);

        let (let_vars, final_call) = collect_lets(&result);
        assert_eq!(let_vars.len(), 2);
        assert_eq!(let_vars[0], "V_Anf0");
        assert_eq!(let_vars[1], "V_Anf1");
        match final_call {
            CErlExpr::Call { args, .. } => {
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], CErlExpr::Var(CErlVar(s)) if s == "V_Anf0"));
                assert!(matches!(&args[1], CErlExpr::Var(CErlVar(s)) if s == "V_Anf1"));
            }
            other => panic!("expected Call, got {other:?}"),
        }
    }

    #[test]
    fn local_fn_ref_not_hoisted() {
        // apply 'f'/1 (V_X) — LocalFnRef is atomic.
        let expr = CErlExpr::Apply {
            callee: Box::new(local_fn_ref("f", 1)),
            args: vec![var("V_X")],
        };
        let mut anf = Anf::new();
        let result = normalise_expr(expr, &mut anf);
        assert!(matches!(result, CErlExpr::Apply { .. }));
    }

    #[test]
    fn tuple_element_hoisted() {
        // {call 'f'(V_X), V_Y} → let V_Anf0 = call 'f'(V_X) in {V_Anf0, V_Y}
        let expr = CErlExpr::Tuple(vec![call("f", "fn", vec![var("V_X")]), var("V_Y")]);
        let mut anf = Anf::new();
        let result = normalise_expr(expr, &mut anf);

        let (let_vars, final_expr) = collect_lets(&result);
        assert_eq!(let_vars.len(), 1);
        assert!(matches!(final_expr, CErlExpr::Tuple(_)));
    }

    #[test]
    fn lit_arg_not_hoisted() {
        // call 'f'('ok') — atom literal is atomic.
        let expr = call("f", "fn", vec![atom("ok")]);
        let mut anf = Anf::new();
        let result = normalise_expr(expr, &mut anf);
        assert!(matches!(result, CErlExpr::Call { .. }));
    }

    #[test]
    fn counter_resets_per_normalise_module() {
        // Each normalise_module call gets a fresh Anf counter.
        // Build a tiny module with one function.
        let inner = call("g", "fn", vec![var("V_X")]);
        let outer = call("f", "fn", vec![inner]);
        let fun_body = CErlExpr::Fun {
            params: vec![CErlVar("V_X".into())],
            body: Box::new(outer),
        };
        let mut module = CErlModule {
            name: CErlAtom("test".into()),
            exports: Vec::new(),
            attributes: Vec::new(),
            fns: vec![CErlFn {
                name: CErlAtom("main".into()),
                arity: 1,
                anns: Vec::new(),
                body: fun_body,
            }],
        };
        normalise_module(&mut module);
        // Should produce V_Anf0 inside the function body (fresh counter from 0).
        let body = &module.fns[0].body;
        // body = Fun { body = Let { var = V_Anf0, ... } }
        match body {
            CErlExpr::Fun { body: fun_body, .. } => {
                assert!(
                    matches!(fun_body.as_ref(), CErlExpr::Let { var, .. } if var.0 == "V_Anf0")
                );
            }
            other => panic!("expected Fun, got {other:?}"),
        }
    }
}
