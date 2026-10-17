//! Result boundaries and the SSA values that cross them.

use crate::core_ast::{CErlAtom, CErlExpr, CErlLit, CErlPat, CErlVar};
use crate::error::CodegenError;
use crate::expr::lower_expr_in_scope;
use crate::scope::{ssa_var, LocalScope};
use ridge_ir::{AssignTarget, IrExpr, IrTimeout};
use rustc_hash::FxHashSet;

/// Lower the Result boundary and join its escaping mutable versions.
pub fn lower(body: &IrExpr, id: u32, scope: &mut LocalScope) -> Result<CErlExpr, CodegenError> {
    let mut assigned = FxHashSet::default();
    assignments(body, &FxHashSet::default(), &mut assigned);
    let mut names: Vec<_> = assigned
        .into_iter()
        .filter(|n| scope.current_index(n).is_some())
        .map(|n| scope.mutable_key(&n).to_owned())
        .collect();
    names.sort();
    let mut inner = scope.clone();
    inner.result_scope_vars.push(names.clone());
    let lowered = lower_expr_in_scope(body, &mut inner)?;
    inner.result_scope_vars.pop();
    if names.is_empty() {
        return Ok(crate::return_::wrap_with_result_catch(lowered));
    }
    let initial: Vec<_> = names
        .iter()
        .map(|n| {
            let base = LocalScope::mutable_base(n);
            let var = scope.mutable_value(n);
            (base, var)
        })
        .collect();
    let packed = pack_success(lowered, &initial);
    let mut state = Vec::new();
    for name in &names {
        let index = scope.join_index(name, &inner);
        state.push(ssa_var(&LocalScope::mutable_base(name), index));
    }
    Ok(CErlExpr::BindTuple {
        result: CErlVar(format!("ResultScope{id}")),
        state,
        value: Box::new(crate::return_::wrap_with_result_state_catch(packed)),
    })
}

/// Capture the state at the exit, rather than at the block's unreachable tail.
pub fn pack_exit(value: CErlExpr, scope: &LocalScope) -> CErlExpr {
    let Some(names) = scope
        .result_scope_vars
        .last()
        .filter(|names| !names.is_empty())
    else {
        return value;
    };
    let mut values = vec![value];
    values.extend(names.iter().map(|n| CErlExpr::Var(scope.mutable_value(n))));
    CErlExpr::Tuple(values)
}

fn update_versions(versions: &mut [(String, CErlVar)], var: &CErlVar) {
    for (base, current) in versions {
        if var.0 == *base
            || var.0.strip_prefix(base.as_str()).is_some_and(|suffix| {
                !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
            })
        {
            *current = var.clone();
        }
    }
}

fn update_pattern(versions: &mut [(String, CErlVar)], pattern: &CErlPat) {
    match pattern {
        CErlPat::Var(var) => update_versions(versions, var),
        CErlPat::Tuple(patterns) => {
            for pat in patterns {
                update_pattern(versions, pat);
            }
        }
        CErlPat::Alias { var, inner } => {
            update_versions(versions, var);
            update_pattern(versions, inner);
        }
        _ => {}
    }
}

/// `BindTuple` exports these versions to its consumer. Its internal bindings,
/// lambda bodies and individual case branches remain lexically isolated.
fn exported_versions(expr: &CErlExpr, versions: &mut [(String, CErlVar)]) {
    match expr {
        CErlExpr::BindTuple { state, .. } => {
            for var in state {
                update_versions(versions, var);
            }
        }
        CErlExpr::Let { value, body, .. } => {
            exported_versions(value, versions);
            exported_versions(body, versions);
        }
        CErlExpr::LetRec { body, .. } => exported_versions(body, versions),
        CErlExpr::Do { first, then } => {
            exported_versions(first, versions);
            exported_versions(then, versions);
        }
        CErlExpr::Call { args, .. }
        | CErlExpr::PrimOp { args, .. }
        | CErlExpr::Tuple(args)
        | CErlExpr::ListLit(args) => {
            for arg in args {
                exported_versions(arg, versions);
            }
        }
        CErlExpr::Apply { callee, args } => {
            exported_versions(callee, versions);
            for arg in args {
                exported_versions(arg, versions);
            }
        }
        CErlExpr::Case { scrutinee, .. } => exported_versions(scrutinee, versions),
        CErlExpr::Cons { head, tail } => {
            exported_versions(head, versions);
            exported_versions(tail, versions);
        }
        CErlExpr::MapLit(pairs) => {
            for (k, v) in pairs {
                exported_versions(k, versions);
                exported_versions(v, versions);
            }
        }
        CErlExpr::MapUpdate { base, updates } => {
            exported_versions(base, versions);
            for (k, v) in updates {
                exported_versions(k, versions);
                exported_versions(v, versions);
            }
        }
        CErlExpr::Fun { .. }
        | CErlExpr::Receive { .. }
        | CErlExpr::Try { .. }
        | CErlExpr::Var(_)
        | CErlExpr::Lit(_)
        | CErlExpr::LocalFnRef { .. } => {}
    }
}

/// Pack at each normal tail, while the bindings for its SSA values are live.
fn pack_success(expr: CErlExpr, incoming: &[(String, CErlVar)]) -> CErlExpr {
    let mut versions = incoming.to_vec();
    match expr {
        CErlExpr::Let { var, value, body } => {
            exported_versions(&value, &mut versions);
            update_versions(&mut versions, &var);
            CErlExpr::Let {
                var,
                value,
                body: Box::new(pack_success(*body, &versions)),
            }
        }
        CErlExpr::Do { first, then } => {
            exported_versions(&first, &mut versions);
            CErlExpr::Do {
                first,
                then: Box::new(pack_success(*then, &versions)),
            }
        }
        CErlExpr::LetRec { defs, body } => CErlExpr::LetRec {
            defs,
            body: Box::new(pack_success(*body, &versions)),
        },
        CErlExpr::Case { scrutinee, clauses } => {
            exported_versions(&scrutinee, &mut versions);
            let mut clauses: Vec<_> = clauses
                .into_iter()
                .map(|mut clause| {
                    let mut versions = versions.clone();
                    update_pattern(&mut versions, &clause.pattern);
                    clause.body = pack_success(clause.body, &versions);
                    clause
                })
                .collect();
            if !clauses.iter().any(|clause| {
                matches!(clause.pattern, CErlPat::Wild | CErlPat::Var(_))
                    && matches!(&clause.guard, CErlExpr::Lit(CErlLit::Atom(a)) if a.0 == "true")
            }) {
                clauses.push(crate::core_ast::CErlClause {
                    pattern: CErlPat::Var(CErlVar("ResultScopeUnmatched".into())),
                    guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                    body: CErlExpr::Call {
                        module: CErlAtom("erlang".into()),
                        fn_name: CErlAtom("error".into()),
                        args: vec![CErlExpr::Tuple(vec![
                            CErlExpr::Lit(CErlLit::Atom(CErlAtom("case_clause".into()))),
                            CErlExpr::Var(CErlVar("ResultScopeUnmatched".into())),
                        ])],
                    },
                });
            }
            CErlExpr::Case { scrutinee, clauses }
        }
        value => {
            exported_versions(&value, &mut versions);
            let mut values = vec![CErlExpr::Tuple(vec![
                CErlExpr::Lit(CErlLit::Atom(CErlAtom("ok".into()))),
                value,
            ])];
            values.extend(versions.into_iter().map(|(_, var)| CErlExpr::Var(var)));
            CErlExpr::Tuple(values)
        }
    }
}

/// Lambdas have their own assignments; shadowed declarations are local.
#[allow(clippy::too_many_lines)]
fn assignments(expr: &IrExpr, shadowed: &FxHashSet<String>, out: &mut FxHashSet<String>) {
    match expr {
        IrExpr::Assign { target, value, .. } => {
            if let AssignTarget::Local { name, .. } = target {
                if !shadowed.contains(name) {
                    out.insert(name.clone());
                }
            }
            assignments(value, shadowed, out);
        }
        IrExpr::VarIn {
            name, value, body, ..
        } => {
            assignments(value, shadowed, out);
            let mut shadowed = shadowed.clone();
            shadowed.insert(name.clone());
            assignments(body, &shadowed, out);
        }
        IrExpr::LetIn { value, body, .. } => {
            assignments(value, shadowed, out);
            assignments(body, shadowed, out);
        }
        IrExpr::ResultScope { body, .. } => assignments(body, shadowed, out),
        IrExpr::ResultExit { value, .. } | IrExpr::Return { value, .. } => {
            assignments(value, shadowed, out);
        }
        IrExpr::Block { stmts, .. } => {
            for e in stmts {
                assignments(e, shadowed, out);
            }
        }
        IrExpr::Match {
            scrutinee, arms, ..
        } => {
            assignments(scrutinee, shadowed, out);
            for arm in arms {
                if let Some(guard) = &arm.when {
                    assignments(guard, shadowed, out);
                }
                assignments(&arm.body, shadowed, out);
            }
        }
        IrExpr::Call { callee, args, .. } => {
            assignments(callee, shadowed, out);
            for a in args {
                assignments(a, shadowed, out);
            }
        }
        IrExpr::Construct { fields, .. } => {
            for (_, e) in fields {
                assignments(e, shadowed, out);
            }
        }
        IrExpr::RecordUpdate { base, updates, .. } => {
            assignments(base, shadowed, out);
            for (_, e) in updates {
                assignments(e, shadowed, out);
            }
        }
        IrExpr::Field { base, .. } => assignments(base, shadowed, out),
        IrExpr::Tuple { elems, .. } | IrExpr::ListLit { elems, .. } => {
            for e in elems {
                assignments(e, shadowed, out);
            }
        }
        IrExpr::Cons { head, tail, .. } => {
            assignments(head, shadowed, out);
            assignments(tail, shadowed, out);
        }
        IrExpr::Ask {
            handle,
            args,
            timeout,
            ..
        }
        | IrExpr::TryAsk {
            handle,
            args,
            timeout,
            ..
        } => {
            assignments(handle, shadowed, out);
            for e in args {
                assignments(e, shadowed, out);
            }
            if let Some(IrTimeout::Millis(e)) = timeout {
                assignments(e, shadowed, out);
            }
        }
        IrExpr::Send { handle, args, .. } | IrExpr::ActorSend { handle, args, .. } => {
            assignments(handle, shadowed, out);
            for e in args {
                assignments(e, shadowed, out);
            }
        }
        IrExpr::Spawn { args, .. } | IrExpr::ChildSpec { args, .. } => {
            for e in args {
                assignments(e, shadowed, out);
            }
        }
        _ => {}
    }
}
