//! Full provider identities preserve head patterns and context correlations.

use std::collections::BTreeMap;
use std::fmt::Write;

use ridge_resolve::ModuleId;
use ridge_typecheck::DictPlan;
use ridge_types::{CapRow, ClassId, RowTail, TyConId, TyVid, Type};

use crate::ctx::LowerCtx;

fn type_name(ctx: &LowerCtx<'_>, tycon: TyConId) -> (String, String) {
    ctx.workspace
        .and_then(|ws| ws.tycons.get(tycon.0 as usize))
        .map_or_else(
            || (String::new(), "unknown".into()),
            |decl| {
                let owner = decl
                    .def_module_raw
                    .and_then(|id| ctx.module_names.get(&ModuleId(id)));
                if owner.is_none_or(|name| name.starts_with("std.")) {
                    // Some library declarations also have a compiler-owned
                    // constructor in consumer workspaces (notably SqlValue).
                    // Their source and seeded forms share the declaration home.
                    if decl.def_module_raw.is_none_or(|module| module == u32::MAX)
                        || ctx.workspace.is_some_and(|workspace| {
                            workspace.stdlib_tycons.get(&decl.name) == Some(&tycon)
                        })
                    {
                        let mut declarations = ridge_resolve::BUILTINS
                            .iter()
                            .filter(|module| module.declared_types.contains(&decl.name.as_str()));
                        if let Some(module) = declarations.next() {
                            if declarations.next().is_none() {
                                return (module.name.into(), decl.name.clone());
                            }
                        }
                    }
                    if let Some(name) = ctx.workspace.and_then(|workspace| {
                        workspace
                            .stdlib_tycons
                            .iter()
                            .filter(|(name, id)| {
                                **id == tycon && name.rsplit('.').next() == Some(decl.name.as_str())
                            })
                            .map(|(name, _)| name)
                            .min()
                    }) {
                        if let Some((module, name)) = name.rsplit_once('.') {
                            return (module.into(), name.into());
                        }
                    }
                }
                (owner.cloned().unwrap_or_default(), decl.name.clone())
            },
        )
}

fn class_name(ctx: &LowerCtx<'_>, class: ClassId) -> String {
    // Prelude declarations reuse reserved class identities even when their
    // source declaration supplies a module for orphan checking. That module
    // does not turn the intrinsic provider into a different class.
    let prelude = match class {
        ridge_types::TOTEXT_CLASS => Some("ToText"),
        ridge_types::EQ_CLASS => Some("Eq"),
        ridge_types::ORD_CLASS => Some("Ord"),
        ridge_types::ENCODE_CLASS => Some("Encode"),
        ridge_types::DECODE_CLASS => Some("Decode"),
        ridge_types::ERROR_FROM_CLASS => Some("ErrorFrom"),
        ridge_types::HASH_CLASS => Some("Hash"),
        _ => None,
    };
    if let Some(name) = prelude {
        return name.into();
    }
    let name = ctx.class_name(class).unwrap_or("Unknown");
    ctx.class_table
        .and_then(|table| table.get(class))
        .and_then(|info| info.def_module)
        .and_then(|module| ctx.module_names.get(&ModuleId(module)))
        .filter(|module| !module.starts_with("std."))
        .map_or_else(|| name.into(), |module| format!("{module}::{name}"))
}

#[derive(Default)]
struct Variables {
    types: BTreeMap<u32, usize>,
    rows: BTreeMap<u32, usize>,
    caps: BTreeMap<u32, usize>,
    unsupported: bool,
}

fn variable(variables: &mut BTreeMap<u32, usize>, id: u32) -> usize {
    let next = variables.len();
    *variables.entry(id).or_insert(next)
}

impl Variables {
    fn capabilities(&mut self, caps: &CapRow) -> String {
        match caps {
            CapRow::Concrete(set) => format!("{set:?}"),
            CapRow::Var(id) => format!("cap{}", variable(&mut self.caps, id.0)),
            CapRow::Union(rows) => {
                let mut parts: Vec<_> = rows.iter().map(|row| self.capabilities(row)).collect();
                parts.sort();
                parts.dedup();
                format!("union{parts:?}")
            }
            _ => {
                self.unsupported = true;
                String::new()
            }
        }
    }

    fn ty(&mut self, ctx: &LowerCtx<'_>, ty: &Type) -> String {
        match ty {
            Type::Var(id) => format!("v{}", variable(&mut self.types, id.0)),
            Type::Rigid { name, .. } => format!("rigid:{name:?}"),
            Type::Con(id, args) => {
                let args: Vec<_> = args.iter().map(|arg| self.ty(ctx, arg)).collect();
                format!("con:{:?}:{args:?}", type_name(ctx, *id))
            }
            Type::Fn { params, ret, caps } => {
                let params: Vec<_> = params.iter().map(|param| self.ty(ctx, param)).collect();
                let ret = self.ty(ctx, ret);
                format!("fn:{params:?}:{ret:?}:{}", self.capabilities(caps))
            }
            Type::Tuple(elems) => {
                let elems: Vec<_> = elems.iter().map(|elem| self.ty(ctx, elem)).collect();
                format!("tuple:{elems:?}")
            }
            Type::Record { fields, tail } => {
                let fields: Vec<_> = fields
                    .iter()
                    .map(|(name, ty)| (name, self.ty(ctx, ty)))
                    .collect();
                let tail = match tail {
                    RowTail::Closed => "closed".into(),
                    RowTail::Open(id) => format!("row{}", variable(&mut self.rows, id.0)),
                    _ => {
                        self.unsupported = true;
                        String::new()
                    }
                };
                format!("record:{fields:?}:{tail}")
            }
            Type::Alias { body, .. } => self.ty(ctx, body),
            Type::Error => "error".into(),
            _ => {
                self.unsupported = true;
                String::new()
            }
        }
    }
}

pub fn recipe(ctx: &LowerCtx<'_>, plan: &DictPlan) -> Option<String> {
    let class_id = crate::core::plan_class(plan);
    let class = class_name(ctx, class_id);
    match plan {
        DictPlan::Structural { fields, args, .. } => {
            Some(format!("{class}:structural:{fields:?}:{}", args.len()))
        }
        DictPlan::Static {
            tycon,
            extra_head,
            info,
            ..
        } => {
            let heads: Vec<_> = std::iter::once(*tycon)
                .chain(extra_head.iter().copied())
                .collect();
            let contract = ctx
                .instance_env
                .and_then(|env| env.evidence_contract(class_id, &heads));
            let canonical_heads =
                contract.map_or(heads.as_slice(), |contract| contract.heads.as_slice());
            let names: Vec<_> = canonical_heads
                .iter()
                .map(|head| type_name(ctx, *head))
                .collect();
            let positions = contract
                .map(|contract| &contract.context_positions)
                .or_else(|| {
                    info.def_module.and_then(|module| {
                        ctx.instance_env
                            .and_then(|env| env.context_arg_positions.get(&(module, info.span)))
                    })
                });
            let context: Vec<_> = info
                .ctx_constraints
                .iter()
                .enumerate()
                .map(|(index, constraint)| {
                    let slots = positions
                        .and_then(|all| all.get(index))
                        .cloned()
                        .unwrap_or_else(|| {
                            info.head_var_positions
                                .get(index)
                                .copied()
                                .into_iter()
                                .collect()
                        });
                    (class_name(ctx, constraint.class), slots)
                })
                .collect();
            let mut identity = format!("{class}:static:{names:?}:{context:?}");
            if let Some(patterns) = contract
                .and_then(|contract| contract.patterns.as_deref())
                .or_else(|| {
                    ctx.instance_env
                        .and_then(|env| env.head_pattern(class_id, &heads))
                })
            {
                let mut actual_variables = Variables::default();
                let actual: Vec<_> = patterns
                    .iter()
                    .map(|ty| actual_variables.ty(ctx, ty))
                    .collect();
                if actual_variables.unsupported {
                    return None;
                }
                let mut next = 0;
                let uniform: Vec<_> = canonical_heads
                    .iter()
                    .map(|head| {
                        let arity = ctx
                            .workspace
                            .and_then(|ws| ws.tycons.get(head.0 as usize))
                            .map_or(0, |decl| decl.arity);
                        let args = (0..arity)
                            .map(|_| {
                                let id = next;
                                next += 1;
                                Type::Var(TyVid(id))
                            })
                            .collect();
                        Type::Con(*head, args)
                    })
                    .collect();
                let mut variables = Variables::default();
                let uniform: Vec<_> = uniform.iter().map(|ty| variables.ty(ctx, ty)).collect();
                if actual != uniform {
                    write!(identity, ":head:{actual:?}").ok()?;
                }
            }
            Some(identity)
        }
        DictPlan::Forward(_) => unreachable!("forwarded evidence carries its provider"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_identity_normalizes_variables_but_preserves_their_correlation() {
        let ctx = LowerCtx::new(ModuleId(0), &[], crate::test_support::builtins());
        let pattern =
            |left, right| Type::Tuple(vec![Type::Var(TyVid(left)), Type::Var(TyVid(right))]);
        let original = Variables::default().ty(&ctx, &pattern(20, 20));
        let renamed = Variables::default().ty(&ctx, &pattern(7, 7));
        let independent = Variables::default().ty(&ctx, &pattern(7, 8));
        assert_eq!(original, renamed);
        assert_ne!(original, independent);
    }
}
