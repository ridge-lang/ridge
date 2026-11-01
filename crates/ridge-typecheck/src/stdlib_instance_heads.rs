//! Source heads shared by seeded consumers and source-built library providers.

use std::sync::LazyLock;

use ridge_ast::{Ident, Span, Type};

const S: Span = Span::point(0);

fn ident(name: &str) -> Ident {
    Ident {
        text: name.into(),
        span: S,
    }
}

type Declaration = (&'static str, Vec<Type>, Vec<ridge_ast::ClassConstraint>);

static DECLARATIONS: LazyLock<Vec<Declaration>> =
    LazyLock::new(|| include!(concat!(env!("OUT_DIR"), "/stdlib_instance_heads.rs")));

pub fn seed(
    builtins: &ridge_types::BuiltinTyCons,
    classes: &crate::class_env::ClassTable,
    env: &mut crate::class_env::InstanceEnv,
    names: &rustc_hash::FxHashMap<String, ridge_types::TyConId>,
    origins: &crate::cross_module::TyConOrigins,
) {
    use crate::class_env::{EvidenceContract, InstanceHead};
    for (name, heads, constraints) in DECLARATIONS.iter() {
        let Some(class) = classes.id_by_name(name) else {
            continue;
        };
        let ids: Option<InstanceHead> = heads
            .iter()
            .map(|head| crate::collect::extract_tycon_id(head, names, origins, builtins))
            .collect();
        let Some(ids) = ids else { continue };
        if env
            .get_multi(class, &ids)
            .is_some_and(|info| info.def_module.is_none())
        {
            env.record_head_asts(class, ids, heads.clone());
        } else if ids.len() > 1 {
            let receiver = InstanceHead::from_slice(&ids[..1]);
            if env
                .get_multi(class, &receiver)
                .is_some_and(|info| info.def_module.is_none())
            {
                if let Ok(context_positions) =
                    crate::collect::source_context_positions(name, heads, constraints, classes)
                {
                    env.evidence_contracts.insert(
                        (class, receiver),
                        EvidenceContract {
                            heads: ids,
                            context_positions,
                            head_asts: heads.clone(),
                            patterns: None,
                        },
                    );
                }
            }
        }
    }
}
