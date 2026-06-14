//! Pure data crate for the Ridge type system.
//!
//! Contains type identifiers, capability sets, type representations, schemes,
//! substitutions, type-constructor declarations, built-in type tables, and
//! exhaustiveness witness shapes. No I/O, no inference.

#![warn(missing_docs)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod builtins;
pub mod capability_set;
pub mod constraint;
pub mod scheme;
pub mod shape_key;
pub mod subst;
pub mod ty;
pub mod tycon;
pub mod witness;

// ── Public re-exports ─────────────────────────────────────────────────────────

pub use builtins::{
    fn_tycon_arity, fn_tycon_id, fn_tycon_name, BuiltinTyCons, FN_ARITY_COUNT, FN_TYCON_BASE,
    RET_TYCON_ID,
};
pub use capability_set::CapabilitySet;
pub use constraint::{
    ClassId, Constraint, DECODE_CLASS, ENCODE_CLASS, EQ_CLASS, ORD_CLASS, TOTEXT_CLASS,
};
pub use scheme::Scheme;
pub use shape_key::{shape_key, type_to_key, AnonRecordTable, CapKey, ShapeKey, TyKey};
pub use subst::Subst;
pub use ty::{CapRow, CapVid, Row, RowTail, RowVid, TyVid, Type};
pub use tycon::{
    ActorSchema, HandlerSchema, RecordField, RecordSchema, TyConArena, TyConDecl, TyConId,
    TyConKind, UnionSchema, UnionVariant, VariantPayload,
};
pub use witness::{MatchWitness, WitnessKind, WitnessPat};
