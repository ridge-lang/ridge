//! Validate complete instance heads without binding the caller's variables.

use ridge_types::{ClassId, RowTail, TyConId, TyVid, Type};
use rustc_hash::FxHashMap;

use crate::class_env::{InstanceEnv, InstanceHead};
use crate::ctx::InferCtx;
use crate::type_template::TypeTemplates;

pub fn prepare(templates: &TypeTemplates<'_>, env: &mut InstanceEnv) {
    for (key, heads) in &env.head_asts {
        let module = env
            .instances
            .get(key)
            .and_then(|info| info.def_module)
            .unwrap_or(u32::MAX);
        let params: Option<FxHashMap<_, _>> = crate::solve::collect_head_vars(heads)
            .into_iter()
            .enumerate()
            .map(|(index, name)| Some((name, Type::Var(TyVid(u32::try_from(index).ok()?)))))
            .collect();
        let patterns = params.and_then(|params| {
            heads
                .iter()
                .map(|head| templates.convert_head(module, head, &params))
                .collect()
        });
        env.head_patterns.insert(key.clone(), patterns);
    }
    for contract in env.evidence_contracts.values_mut() {
        let params: FxHashMap<_, _> = crate::solve::collect_head_vars(&contract.head_asts)
            .into_iter()
            .enumerate()
            .map(|(index, name)| {
                (
                    name,
                    Type::Var(TyVid(u32::try_from(index).unwrap_or(u32::MAX))),
                )
            })
            .collect();
        contract.patterns = contract
            .head_asts
            .iter()
            .map(|head| templates.convert_head(u32::MAX, head, &params))
            .collect();
    }
}

pub fn accepts(
    ctx: &mut InferCtx,
    env: &InstanceEnv,
    class: ClassId,
    head: &[TyConId],
    actual: &[Type],
) -> bool {
    let key = (class, InstanceHead::from_slice(head));
    let Some(patterns) = env.head_patterns.get(&key) else {
        return true;
    };
    let Some(patterns) = patterns else {
        return false;
    };
    if patterns.len() != actual.len() {
        return false;
    }
    let actual: Vec<_> = actual
        .iter()
        .map(|ty| {
            let ty = ctx.deep_resolve(ty);
            crate::tycon_collect::chase_alias_chain(&ctx.tycon_decls, &ty, &mut vec![])
        })
        .collect();
    let mut bindings = FxHashMap::default();
    patterns
        .iter()
        .zip(&actual)
        .all(|(pattern, actual)| matches_type(pattern, actual, &mut bindings))
}

fn peel(ty: &Type) -> &Type {
    match ty {
        Type::Alias { body, .. } => peel(body),
        _ => ty,
    }
}

pub fn determined(
    patterns: Option<&[Type]>,
    from: &[usize],
    actual: &[Type],
) -> Option<Vec<Option<Type>>> {
    let patterns = patterns?;
    let mut bindings = FxHashMap::default();
    for &position in from {
        if !matches_type(
            patterns.get(position)?,
            actual.get(position)?,
            &mut bindings,
        ) {
            return None;
        }
    }
    Some(
        patterns
            .iter()
            .map(|pattern| instantiate(pattern, &bindings))
            .collect(),
    )
}

fn instantiate(pattern: &Type, bindings: &FxHashMap<TyVid, Type>) -> Option<Type> {
    match peel(pattern) {
        Type::Var(var) => bindings.get(var).cloned(),
        Type::Con(id, args) => Some(Type::Con(
            *id,
            args.iter()
                .map(|arg| instantiate(arg, bindings))
                .collect::<Option<_>>()?,
        )),
        Type::Tuple(elems) => Some(Type::Tuple(
            elems
                .iter()
                .map(|elem| instantiate(elem, bindings))
                .collect::<Option<_>>()?,
        )),
        Type::Fn { params, ret, caps } => Some(Type::Fn {
            params: params
                .iter()
                .map(|param| instantiate(param, bindings))
                .collect::<Option<_>>()?,
            ret: Box::new(instantiate(ret, bindings)?),
            caps: caps.clone(),
        }),
        Type::Record {
            fields,
            tail: RowTail::Closed,
        } => Some(Type::record(
            fields
                .iter()
                .map(|(name, ty)| Some((name.clone(), instantiate(ty, bindings)?)))
                .collect::<Option<_>>()?,
            RowTail::Closed,
        )),
        _ => None,
    }
}

fn same(left: &Type, right: &Type) -> bool {
    match (peel(left), peel(right)) {
        (Type::Var(a), Type::Var(b)) => a == b,
        (Type::Rigid { id: a, .. }, Type::Rigid { id: b, .. }) => a == b,
        (Type::Con(a, xs), Type::Con(b, ys)) => a == b && same_many(xs, ys),
        (Type::Tuple(xs), Type::Tuple(ys)) => same_many(xs, ys),
        (
            Type::Fn {
                params: xs,
                ret: x,
                caps: a,
            },
            Type::Fn {
                params: ys,
                ret: y,
                caps: b,
            },
        ) => a == b && same_many(xs, ys) && same(x, y),
        (
            Type::Record {
                fields: xs,
                tail: a,
            },
            Type::Record {
                fields: ys,
                tail: b,
            },
        ) => {
            a == b
                && xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys)
                    .all(|((a, x), (b, y))| a == b && same(x, y))
        }
        _ => false,
    }
}

fn same_many(left: &[Type], right: &[Type]) -> bool {
    left.len() == right.len() && left.iter().zip(right).all(|(a, b)| same(a, b))
}

fn matches_type(pattern: &Type, actual: &Type, bindings: &mut FxHashMap<TyVid, Type>) -> bool {
    match (peel(pattern), peel(actual)) {
        (Type::Var(var), actual) => match bindings.entry(*var) {
            std::collections::hash_map::Entry::Occupied(entry) => same(entry.get(), actual),
            std::collections::hash_map::Entry::Vacant(entry) => {
                entry.insert(actual.clone());
                true
            }
        },
        (Type::Con(a, xs), Type::Con(b, ys)) if a == b => matches_many(xs, ys, bindings),
        (Type::Tuple(xs), Type::Tuple(ys)) => matches_many(xs, ys, bindings),
        (
            Type::Fn {
                params: xs,
                ret: x,
                caps: a,
            },
            Type::Fn {
                params: ys,
                ret: y,
                caps: b,
            },
        ) => a == b && matches_many(xs, ys, bindings) && matches_type(x, y, bindings),
        (
            Type::Record { fields: xs, tail },
            Type::Record {
                fields: ys,
                tail: actual_tail,
            },
        ) => {
            for (label, pattern) in xs {
                let Some((_, actual)) = ys.iter().find(|(name, _)| name == label) else {
                    return false;
                };
                if !matches_type(pattern, actual, bindings) {
                    return false;
                }
            }
            match tail {
                RowTail::Closed => xs.len() == ys.len() && *actual_tail == RowTail::Closed,
                RowTail::Open(var) => {
                    let remaining = Type::record(
                        ys.iter()
                            .filter(|(name, _)| !xs.iter().any(|(label, _)| label == name))
                            .cloned()
                            .collect(),
                        actual_tail.clone(),
                    );
                    matches_type(&Type::Var(TyVid(var.0)), &remaining, bindings)
                }
                _ => false,
            }
        }
        _ => false,
    }
}

fn matches_many(pattern: &[Type], actual: &[Type], bindings: &mut FxHashMap<TyVid, Type>) -> bool {
    pattern.len() == actual.len()
        && pattern
            .iter()
            .zip(actual)
            .all(|(p, a)| matches_type(p, a, bindings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_types::{CapRow, CapabilitySet};

    fn function(param: Type, ret: Type, caps: CapabilitySet) -> Type {
        Type::Fn {
            params: vec![param],
            ret: Box::new(ret),
            caps: CapRow::Concrete(caps),
        }
    }

    #[test]
    fn function_heads_check_parameters_return_and_capabilities() {
        let ctx = InferCtx::for_tests();
        let int = Type::Con(ctx.builtins.int, vec![]);
        let bool_ty = Type::Con(ctx.builtins.bool, vec![]);
        let pattern = function(int.clone(), bool_ty.clone(), CapabilitySet::PURE);
        assert!(matches_type(&pattern, &pattern, &mut FxHashMap::default()));
        for actual in [
            function(bool_ty.clone(), bool_ty.clone(), CapabilitySet::PURE),
            function(int.clone(), int.clone(), CapabilitySet::PURE),
            function(
                int,
                bool_ty.clone(),
                CapabilitySet::singleton(ridge_ast::Capability::Io),
            ),
            function(Type::Var(TyVid(77)), bool_ty, CapabilitySet::PURE),
        ] {
            assert!(
                !matches_type(&pattern, &actual, &mut FxHashMap::default()),
                "{actual:?}"
            );
        }
    }

    #[test]
    fn repeated_head_variables_preserve_the_callers_distinct_variables() {
        let pattern = Type::Tuple(vec![Type::Var(TyVid(0)), Type::Var(TyVid(0))]);
        let same_actual = Type::Tuple(vec![Type::Var(TyVid(9)), Type::Var(TyVid(9))]);
        let distinct_actual = Type::Tuple(vec![Type::Var(TyVid(9)), Type::Var(TyVid(10))]);
        assert!(matches_type(
            &pattern,
            &same_actual,
            &mut FxHashMap::default()
        ));
        assert!(!matches_type(
            &pattern,
            &distinct_actual,
            &mut FxHashMap::default()
        ));
    }

    #[test]
    fn determining_head_cannot_specialize_a_free_caller_parameter() {
        let ctx = InferCtx::for_tests();
        let int = Type::Con(ctx.builtins.int, vec![]);
        let patterns = Some(vec![
            Type::Con(ctx.builtins.list, vec![int.clone()]),
            int.clone(),
        ]);
        let open = vec![
            Type::Con(ctx.builtins.list, vec![Type::Var(TyVid(9))]),
            Type::Var(TyVid(10)),
        ];
        assert!(determined(patterns.as_deref(), &[0], &open).is_none());
        let fixed = vec![
            Type::Con(ctx.builtins.list, vec![int]),
            Type::Var(TyVid(10)),
        ];
        assert!(determined(patterns.as_deref(), &[0], &fixed).unwrap()[1].is_some());
    }
}
