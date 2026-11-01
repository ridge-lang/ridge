//! Keep inferred field obligations available while pinning call dictionaries.

use std::sync::Arc;

use ridge_types::{Constraint, RecordRequirement, Scheme, Type};

#[derive(Clone)]
pub struct DictionarySignature {
    pub constraints: Vec<Constraint>,
    pub params: Vec<Type>,
    pub ret: Type,
    pub record_fields: Option<Arc<Vec<RecordRequirement>>>,
}

impl DictionarySignature {
    pub fn from_scheme(scheme: &Scheme) -> Self {
        let (params, ret) = match &scheme.ty {
            Type::Fn { params, ret, .. } => (params.clone(), (**ret).clone()),
            _ => (Vec::new(), Type::Error),
        };
        Self {
            constraints: scheme.constraints.clone(),
            params,
            ret,
            record_fields: scheme.record_fields.clone(),
        }
    }
}
