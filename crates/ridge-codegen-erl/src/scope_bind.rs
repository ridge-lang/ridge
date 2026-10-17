//! Extend tuple-state bindings over their consumers before ordinary ANF.
//!
//! A binding inside a call argument or a let initializer must enclose the
//! continuation, rather than disappear when that child produces its value.

use crate::core_ast::{CErlAtom, CErlClause, CErlExpr, CErlLit, CErlPat, CErlVar};
use rustc_hash::FxHashMap;

enum Binding {
    Let(CErlVar, CErlExpr),
    Tuple(Vec<CErlVar>, CErlExpr),
    Do(CErlExpr),
    Rec(Vec<(CErlAtom, u32, CErlExpr)>),
}

#[cfg(all(test, feature = "beam-runtime"))]
mod tests {
    use crate::core_ast::*;

    #[test]
    fn state_binding_does_not_capture_an_operand_local() {
        let result = CErlVar("ResultScope0".into());
        let state = CErlVar("V_N2".into());
        let x = CErlVar("V_X".into());
        let literal = |n| CErlExpr::Lit(CErlLit::Int(n));
        let body = CErlExpr::Let {
            var: x.clone(),
            value: Box::new(literal(1)),
            body: Box::new(CErlExpr::Tuple(vec![
                CErlExpr::Let {
                    var: x.clone(),
                    value: Box::new(literal(2)),
                    body: Box::new(CErlExpr::BindTuple {
                        result,
                        state: vec![state.clone()],
                        value: Box::new(CErlExpr::Tuple(vec![
                            CErlExpr::Var(x.clone()),
                            literal(3),
                        ])),
                    }),
                },
                CErlExpr::Var(x),
                CErlExpr::Var(state),
            ])),
        };
        let mut module = CErlModule {
            name: CErlAtom("ridge_scope_binding".into()),
            exports: vec![CErlExport {
                name: CErlAtom("run".into()),
                arity: 0,
            }],
            attributes: vec![],
            fns: vec![CErlFn {
                name: CErlAtom("run".into()),
                arity: 0,
                anns: vec![],
                body: CErlExpr::Fun {
                    params: vec![],
                    body: Box::new(body),
                },
            }],
        };
        crate::anf::normalise_module(&mut module);
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("ridge_scope_binding.core");
        std::fs::write(&source, crate::printer::print_module(&module)).unwrap();
        let compile = std::process::Command::new("erlc")
            .arg("+from_core")
            .arg("-o")
            .arg(dir.path())
            .arg(&source)
            .output()
            .expect("erlc required");
        assert!(
            compile.status.success(),
            "{}",
            String::from_utf8_lossy(&compile.stdout)
        );
        let run = std::process::Command::new("erl").arg("-noshell").arg("-pa").arg(dir.path())
            .args(["-eval", "case ridge_scope_binding:run() of {2,1,3} -> io:put_chars(\"scope-ok\"); Other -> io:format(\"~p\",[Other]),halt(5) end,halt()."])
            .output().expect("erl required");
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stdout)
        );
        assert_eq!(run.stdout, b"scope-ok");
    }
}

struct Value {
    bindings: Vec<Binding>,
    expr: CErlExpr,
}

impl Value {
    fn seal(self) -> CErlExpr {
        self.bindings
            .into_iter()
            .rev()
            .fold(self.expr, |body, binding| match binding {
                Binding::Let(var, value) => CErlExpr::Let {
                    var,
                    value: Box::new(value),
                    body: Box::new(body),
                },
                Binding::Tuple(vars, value) => CErlExpr::Case {
                    scrutinee: Box::new(value),
                    clauses: vec![
                        CErlClause {
                            pattern: CErlPat::Tuple(vars.into_iter().map(CErlPat::Var).collect()),
                            guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                            body,
                        },
                        CErlClause {
                            pattern: CErlPat::Wild,
                            guard: CErlExpr::Lit(CErlLit::Atom(CErlAtom("true".into()))),
                            body: CErlExpr::Call {
                                module: CErlAtom("erlang".into()),
                                fn_name: CErlAtom("error".into()),
                                args: vec![CErlExpr::Lit(CErlLit::Atom(CErlAtom(
                                    "invalid_result_scope_state".into(),
                                )))],
                            },
                        },
                    ],
                },
                Binding::Do(first) => CErlExpr::Do {
                    first: Box::new(first),
                    then: Box::new(body),
                },
                Binding::Rec(defs) => CErlExpr::LetRec {
                    defs,
                    body: Box::new(body),
                },
            })
    }
}

/// Resolve tuple bindings without rewriting functions that do not contain one.
pub fn resolve(expr: CErlExpr) -> CErlExpr {
    if contains_binding(&expr) {
        Resolver {
            next: 0,
            vars: FxHashMap::default(),
            fns: FxHashMap::default(),
        }
        .value(expr)
        .seal()
    } else {
        expr
    }
}

fn contains_binding(expr: &CErlExpr) -> bool {
    match expr {
        CErlExpr::BindTuple { .. } => true,
        CErlExpr::Fun { body, .. } => contains_binding(body),
        CErlExpr::Apply { callee, args } => {
            contains_binding(callee) || args.iter().any(contains_binding)
        }
        CErlExpr::Call { args, .. }
        | CErlExpr::PrimOp { args, .. }
        | CErlExpr::Tuple(args)
        | CErlExpr::ListLit(args) => args.iter().any(contains_binding),
        CErlExpr::Let { value, body, .. } => contains_binding(value) || contains_binding(body),
        CErlExpr::LetRec { defs, body } => {
            defs.iter().any(|(_, _, f)| contains_binding(f)) || contains_binding(body)
        }
        CErlExpr::Case { scrutinee, clauses } => {
            contains_binding(scrutinee) || clauses.iter().any(clause_has_binding)
        }
        CErlExpr::Do { first, then } => contains_binding(first) || contains_binding(then),
        CErlExpr::Cons { head, tail } => contains_binding(head) || contains_binding(tail),
        CErlExpr::MapLit(pairs) => pairs
            .iter()
            .any(|(k, v)| contains_binding(k) || contains_binding(v)),
        CErlExpr::MapUpdate { base, updates } => {
            contains_binding(base)
                || updates
                    .iter()
                    .any(|(k, v)| contains_binding(k) || contains_binding(v))
        }
        CErlExpr::Receive { clauses, after } => {
            clauses.iter().any(clause_has_binding)
                || after
                    .as_ref()
                    .is_some_and(|(t, b)| contains_binding(t) || contains_binding(b))
        }
        CErlExpr::Try { body, of, catch } => {
            contains_binding(body) || of.iter().chain(catch).any(clause_has_binding)
        }
        CErlExpr::Lit(_) | CErlExpr::Var(_) | CErlExpr::LocalFnRef { .. } => false,
    }
}

fn clause_has_binding(clause: &CErlClause) -> bool {
    contains_binding(&clause.guard) || contains_binding(&clause.body)
}

struct Resolver {
    next: u32,
    vars: FxHashMap<String, CErlVar>,
    fns: FxHashMap<(String, u32), CErlAtom>,
}

impl Resolver {
    // Source variables always start with V_, so this namespace is disjoint.
    fn fresh(&mut self) -> CErlVar {
        let var = CErlVar(format!("ScopeTmp{}", self.next));
        self.next += 1;
        var
    }

    /// Seal a control or function boundary without leaking its local bindings.
    fn scoped(&mut self, expr: CErlExpr) -> CErlExpr {
        let vars = self.vars.clone();
        let fns = self.fns.clone();
        let body = self.value(expr).seal();
        self.vars = vars;
        self.fns = fns;
        body
    }

    fn bind(&mut self, original: CErlVar) -> CErlVar {
        let var = self.fresh();
        self.vars.insert(original.0, var.clone());
        var
    }

    fn atomic(&mut self, expr: CErlExpr, bindings: &mut Vec<Binding>) -> CErlExpr {
        let value = self.value(expr);
        bindings.extend(value.bindings);
        if matches!(
            value.expr,
            CErlExpr::Var(_) | CErlExpr::Lit(_) | CErlExpr::LocalFnRef { .. }
        ) {
            value.expr
        } else {
            let var = self.fresh();
            bindings.push(Binding::Let(var.clone(), value.expr));
            CErlExpr::Var(var)
        }
    }

    fn args(&mut self, args: Vec<CErlExpr>, bindings: &mut Vec<Binding>) -> Vec<CErlExpr> {
        args.into_iter()
            .map(|arg| self.atomic(arg, bindings))
            .collect()
    }

    fn clause(&mut self, clause: CErlClause) -> CErlClause {
        let vars = self.vars.clone();
        let pattern = self.pattern(clause.pattern, &mut FxHashMap::default());
        let clause = CErlClause {
            pattern,
            guard: self.value(clause.guard).seal(),
            body: self.value(clause.body).seal(),
        };
        self.vars = vars;
        clause
    }

    fn pattern_var(
        &mut self,
        original: CErlVar,
        names: &mut FxHashMap<String, CErlVar>,
    ) -> CErlVar {
        if let Some(var) = names.get(&original.0) {
            return var.clone();
        }
        let name = original.0.clone();
        let var = self.bind(original);
        names.insert(name, var.clone());
        var
    }

    fn pattern(&mut self, pat: CErlPat, names: &mut FxHashMap<String, CErlVar>) -> CErlPat {
        match pat {
            CErlPat::Var(var) => CErlPat::Var(self.pattern_var(var, names)),
            CErlPat::Tuple(pats) => {
                CErlPat::Tuple(pats.into_iter().map(|p| self.pattern(p, names)).collect())
            }
            CErlPat::Cons { head, tail } => CErlPat::Cons {
                head: Box::new(self.pattern(*head, names)),
                tail: Box::new(self.pattern(*tail, names)),
            },
            CErlPat::Alias { var, inner } => CErlPat::Alias {
                var: self.pattern_var(var, names),
                inner: Box::new(self.pattern(*inner, names)),
            },
            CErlPat::MapPat(pairs) => CErlPat::MapPat(
                pairs
                    .into_iter()
                    .map(|(k, p)| (self.scoped(k), self.pattern(p, names)))
                    .collect(),
            ),
            other @ (CErlPat::Wild | CErlPat::Lit(_)) => other,
        }
    }

    #[allow(clippy::too_many_lines)]
    fn value(&mut self, expr: CErlExpr) -> Value {
        let mut bindings = Vec::new();
        let expr = match expr {
            CErlExpr::BindTuple {
                result,
                state,
                value,
            } => {
                let value = self.value(*value);
                bindings.extend(value.bindings);
                let result = self.bind(result);
                let mut vars = vec![result.clone()];
                vars.extend(state.into_iter().map(|var| self.bind(var)));
                bindings.push(Binding::Tuple(vars, value.expr));
                CErlExpr::Var(result)
            }
            CErlExpr::Let { var, value, body } => {
                let value = self.value(*value);
                bindings.extend(value.bindings);
                // Widening this binding must not capture a same-named variable
                // in a later operand. References are renamed while still in
                // their original lexical scope.
                let name = var.0.clone();
                let previous = self.vars.get(&name).cloned();
                let var = self.bind(var);
                bindings.push(Binding::Let(var, value.expr));
                let body = self.value(*body);
                if let Some(previous) = previous {
                    self.vars.insert(name, previous);
                } else {
                    self.vars.remove(&name);
                }
                bindings.extend(body.bindings);
                body.expr
            }
            CErlExpr::Do { first, then } => {
                let first = self.value(*first);
                bindings.extend(first.bindings);
                bindings.push(Binding::Do(first.expr));
                let then = self.value(*then);
                bindings.extend(then.bindings);
                then.expr
            }
            CErlExpr::LetRec { defs, body } => {
                let previous = self.fns.clone();
                for (name, arity, _) in &defs {
                    let fresh = CErlAtom(self.fresh().0);
                    self.fns.insert((name.0.clone(), *arity), fresh);
                }
                let defs = defs
                    .into_iter()
                    .map(|(n, a, f)| {
                        let name = self.fns.get(&(n.0.clone(), a)).cloned().unwrap_or(n);
                        (name, a, self.scoped(f))
                    })
                    .collect();
                bindings.push(Binding::Rec(defs));
                let body = self.value(*body);
                self.fns = previous;
                bindings.extend(body.bindings);
                body.expr
            }
            CErlExpr::Fun { params, body } => {
                let previous = self.vars.clone();
                let params = params.into_iter().map(|param| self.bind(param)).collect();
                let body = self.value(*body).seal();
                self.vars = previous;
                CErlExpr::Fun {
                    params,
                    body: Box::new(body),
                }
            }
            CErlExpr::Call {
                module,
                fn_name,
                args,
            } => CErlExpr::Call {
                module,
                fn_name,
                args: self.args(args, &mut bindings),
            },
            CErlExpr::PrimOp { name, args } => CErlExpr::PrimOp {
                name,
                args: self.args(args, &mut bindings),
            },
            CErlExpr::Apply { callee, args } => CErlExpr::Apply {
                callee: Box::new(self.atomic(*callee, &mut bindings)),
                args: self.args(args, &mut bindings),
            },
            CErlExpr::Tuple(elems) => CErlExpr::Tuple(self.args(elems, &mut bindings)),
            CErlExpr::ListLit(elems) => CErlExpr::ListLit(self.args(elems, &mut bindings)),
            CErlExpr::Cons { head, tail } => CErlExpr::Cons {
                head: Box::new(self.atomic(*head, &mut bindings)),
                tail: Box::new(self.atomic(*tail, &mut bindings)),
            },
            CErlExpr::MapLit(pairs) => CErlExpr::MapLit(
                pairs
                    .into_iter()
                    .map(|(k, v)| (self.atomic(k, &mut bindings), self.atomic(v, &mut bindings)))
                    .collect(),
            ),
            CErlExpr::MapUpdate { base, updates } => CErlExpr::MapUpdate {
                base: Box::new(self.atomic(*base, &mut bindings)),
                updates: updates
                    .into_iter()
                    .map(|(k, v)| (self.atomic(k, &mut bindings), self.atomic(v, &mut bindings)))
                    .collect(),
            },
            CErlExpr::Case { scrutinee, clauses } => CErlExpr::Case {
                scrutinee: Box::new(self.atomic(*scrutinee, &mut bindings)),
                clauses: clauses.into_iter().map(|c| self.clause(c)).collect(),
            },
            CErlExpr::Try { body, of, catch } => CErlExpr::Try {
                body: Box::new(self.scoped(*body)),
                of: of.into_iter().map(|c| self.clause(c)).collect(),
                catch: catch.into_iter().map(|c| self.clause(c)).collect(),
            },
            CErlExpr::Receive { clauses, after } => CErlExpr::Receive {
                clauses: clauses.into_iter().map(|c| self.clause(c)).collect(),
                after: after.map(|(t, b)| (Box::new(self.scoped(*t)), Box::new(self.scoped(*b)))),
            },
            CErlExpr::Var(var) => CErlExpr::Var(self.vars.get(&var.0).cloned().unwrap_or(var)),
            CErlExpr::LocalFnRef { name, arity } => CErlExpr::LocalFnRef {
                name: self
                    .fns
                    .get(&(name.0.clone(), arity))
                    .cloned()
                    .unwrap_or(name),
                arity,
            },
            other @ CErlExpr::Lit(_) => other,
        };
        Value { bindings, expr }
    }
}
