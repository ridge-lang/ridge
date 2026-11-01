//! Verify the component contracts of compiler-owned value records.

use ridge_ast::Span;
use ridge_types::{
    BuiltinTyCons, Constraint, TyConDecl, TyConKind, TyVid, EQ_CLASS, HASH_CLASS, ORD_CLASS,
};
use rustc_hash::FxHashSet;

use crate::class_env::{ClassTable, InstanceEnv};
use crate::ctx::{InferCtx, TyValue};
use crate::error::{TypeDesc, TypeError};
use crate::solve::{solve_constraints, DictPlan};

/// Complete the prelude's value-record instances with checked field evidence.
/// Call after registering the prelude instances and before checking user code.
pub fn prepare_value_comparisons(
    declarations: &[TyConDecl],
    builtins: &BuiltinTyCons,
    classes: &ClassTable,
    instances: &mut InstanceEnv,
) -> Vec<TypeError> {
    let mut errors = Vec::new();
    let mut ctx = InferCtx::new(builtins);
    ctx.tycon_decls = declarations.to_vec();
    for (class, tycon) in [
        (EQ_CLASS, builtins.error),
        (EQ_CLASS, builtins.duration),
        (EQ_CLASS, builtins.proc_output),
        (EQ_CLASS, builtins.cli_parsed),
        (ORD_CLASS, builtins.duration),
        (HASH_CLASS, builtins.error),
        (HASH_CLASS, builtins.duration),
        (HASH_CLASS, builtins.proc_output),
    ] {
        instances.structural_comparisons.remove(&(class, tycon));
        let TyConKind::Record(schema) = &declarations[tycon.0 as usize].kind else {
            errors.push(invalid_value_contract(
                classes,
                class,
                &declarations[tycon.0 as usize].name,
            ));
            continue;
        };
        let requests: Vec<_> = schema
            .record_fields()
            .iter()
            .map(|field| {
                let key = ctx.tyvids.new_key(TyValue(Some(field.ty.clone())));
                Constraint::single(class, TyVid(key.0))
            })
            .collect();
        ctx.deferred_constraints.extend(requests.iter().cloned());
        let (retained, plans) = solve_constraints(
            &mut ctx,
            instances,
            classes,
            &FxHashSet::default(),
            Span::point(0),
            Some(builtins),
        );
        let fields: Option<Vec<_>> = requests
            .iter()
            .map(|request| plans.get(request).cloned())
            .collect();
        if ctx.errors.is_empty() && retained.is_empty() {
            if let Some(args) = fields {
                instances.structural_comparisons.insert(
                    (class, tycon),
                    DictPlan::Structural {
                        class,
                        fields: Some(
                            schema
                                .record_fields()
                                .iter()
                                .map(|field| field.name.clone())
                                .collect(),
                        ),
                        args,
                    },
                );
            }
        }
        if ctx.errors.is_empty()
            && !instances
                .structural_comparisons
                .contains_key(&(class, tycon))
        {
            errors.push(invalid_value_contract(
                classes,
                class,
                &declarations[tycon.0 as usize].name,
            ));
        }
        errors.append(&mut ctx.errors);
    }
    errors
}

fn invalid_value_contract(
    classes: &ClassTable,
    class: ridge_types::ClassId,
    name: &str,
) -> TypeError {
    TypeError::NoInstance {
        class: classes
            .get(class)
            .map_or("?", |info| info.name.as_str())
            .into(),
        ty: TypeDesc::Text(name.into()),
        span: Span::point(0),
        fix_hint: "the core value's declared fields need supported comparison evidence".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::class_env::{register_prelude_classes, register_prelude_instances};
    use ridge_types::{RecordField, RecordSchema, TyConArena, Type};

    #[test]
    fn a_value_record_does_not_keep_evidence_for_an_unsupported_field() {
        let mut arena = TyConArena::new();
        let builtins = BuiltinTyCons::allocate(&mut arena);
        let mut classes = ClassTable::new();
        register_prelude_classes(&mut classes);
        let mut instances = InstanceEnv::new();
        register_prelude_instances(&mut instances, &builtins);
        assert!(
            prepare_value_comparisons(arena.all(), &builtins, &classes, &mut instances).is_empty()
        );
        assert!(instances
            .structural_comparisons
            .contains_key(&(EQ_CLASS, builtins.duration)));
        let mut declarations = arena.all().to_vec();
        declarations[builtins.duration.0 as usize].kind = TyConKind::Record(RecordSchema::new(
            vec![],
            vec![RecordField {
                name: "ms".into(),
                ty: Type::Con(builtins.float, vec![]),
            }],
        ));
        let errors = prepare_value_comparisons(&declarations, &builtins, &classes, &mut instances);
        assert!(!errors.is_empty());
        assert!(!instances
            .structural_comparisons
            .contains_key(&(EQ_CLASS, builtins.duration)));
        assert!(!instances
            .structural_comparisons
            .contains_key(&(ORD_CLASS, builtins.duration)));
    }
}
