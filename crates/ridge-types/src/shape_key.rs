//! Shape-based canonicalisation for anonymous record types.
//!
//! An anonymous (inline) record type `{ x: Int, y: Int }` is interned in the
//! type-constructor arena as a unique [`TyConId`].  Two occurrences of the same
//! structural shape anywhere in the workspace must share one id — independent of
//! source field order and of spelling differences attributable to aliases.
//!
//! [`ShapeKey`] is the canonical, hashable representation used as the key in
//! [`AnonRecordTable`].  [`shape_key`] sorts fields by name and projects each
//! field type to a [`TyKey`] (the hashable projection of a
//! `ridge_types::Type`).
//!
//! # Contract for callers
//!
//! Callers **must** pass fully-resolved types (types that have been through
//! `deep_resolve` on the typecheck side, or that are already concrete on the
//! lower side).  [`type_to_key`] performs only structural projection and
//! alias-peel; it does not resolve unification variables.  Passing an
//! unresolved `Type::Var` produces a [`TyKey::Var`] entry which correctly
//! identifies the unresolved case but will not match the eventual resolved type.

use rustc_hash::FxHashMap;

use crate::{
    ty::{CapRow, TyVid},
    tycon::TyConId,
    Type,
};

// ── TyKey ─────────────────────────────────────────────────────────────────────

/// A hashable structural projection of a [`Type`].
///
/// [`Type`] provides `PartialEq` + `Eq` but not `Hash`; `TyKey` adds `Hash` so
/// that shape maps can use [`FxHashMap`].
///
/// Aliases are peeled to their body before building a `TyKey`, so `{ x: Age }`
/// and `{ x: Int }` produce the same key when `type Age = Int`.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKey {
    /// A type-constructor application.
    Con(TyConId, Vec<Self>),
    /// A structural tuple.
    Tuple(Vec<Self>),
    /// A function type.
    Fn {
        /// Parameter type keys.
        params: Vec<Self>,
        /// Return type key.
        ret: Box<Self>,
        /// Capability-row key.
        caps: CapKey,
    },
    /// An unresolved unification variable.
    Var(TyVid),
    /// The absorbing error type.
    Error,
}

/// Hashable projection of a [`CapRow`].
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum CapKey {
    /// A concrete capability set, represented as its bitmask.
    Concrete(u16),
    /// A capability-row variable.
    Var(u32),
}

// ── ShapeKey ──────────────────────────────────────────────────────────────────

/// The canonical, hashable key for an anonymous record shape.
///
/// Fields are sorted by name in ascending order before being stored, so
/// `{ a: Int, b: Text }` and `{ b: Text, a: Int }` produce identical keys.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ShapeKey(pub Vec<(String, TyKey)>);

// ── AnonRecordTable ───────────────────────────────────────────────────────────

/// Maps an anonymous record shape to the [`TyConId`] that was interned for it.
///
/// Built during the typecheck collect pre-scan and frozen into
/// `TypedWorkspace` so that lowering can resolve inline record types by shape
/// without re-minting ids.
pub type AnonRecordTable = FxHashMap<ShapeKey, TyConId>;

// ── Canonicalisation functions ────────────────────────────────────────────────

/// Project a single [`Type`] to a [`TyKey`], peeling any alias wrapper.
///
/// # Contract
///
/// The caller must supply a fully-resolved type (`deep_resolve`d on the
/// typecheck side; already concrete on the lower side).  This function does
/// **not** resolve unification variables.
pub fn type_to_key(ty: &Type) -> TyKey {
    match ty {
        // Alias peel: recurse on the body, ignoring the alias name.
        Type::Alias { body, .. } => type_to_key(body),
        Type::Con(id, args) => TyKey::Con(*id, args.iter().map(type_to_key).collect()),
        Type::Tuple(elems) => TyKey::Tuple(elems.iter().map(type_to_key).collect()),
        Type::Fn { params, ret, caps } => TyKey::Fn {
            params: params.iter().map(type_to_key).collect(),
            ret: Box::new(type_to_key(ret)),
            caps: cap_row_to_key(caps),
        },
        Type::Var(v) => TyKey::Var(*v),
        Type::Error => TyKey::Error,
    }
}

#[allow(clippy::missing_const_for_fn)]
fn cap_row_to_key(cap_row: &CapRow) -> CapKey {
    use crate::ty::CapVid;
    match cap_row {
        CapRow::Concrete(cs) => CapKey::Concrete(cs.bits()),
        CapRow::Var(CapVid(v)) => CapKey::Var(*v),
        // CapRow is #[non_exhaustive]; treat any future variants as Concrete(0).
        #[allow(unreachable_patterns)]
        _ => CapKey::Concrete(0),
    }
}

/// Build a [`ShapeKey`] from a slice of `(field_name, resolved_type)` pairs.
///
/// The field list is sorted by name in ascending order so that source-order
/// differences between two occurrences of the same shape produce identical keys.
///
/// # Contract
///
/// Each `Type` in `fields` must be fully resolved (see [`type_to_key`]).
#[must_use]
pub fn shape_key(fields: &[(String, Type)]) -> ShapeKey {
    let mut v: Vec<(String, TyKey)> = fields
        .iter()
        .map(|(name, ty)| (name.clone(), type_to_key(ty)))
        .collect();
    v.sort_by(|a, b| a.0.cmp(&b.0));
    ShapeKey(v)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tycon::TyConId;

    fn int_ty() -> Type {
        Type::Con(TyConId(0), vec![])
    }

    fn text_ty() -> Type {
        Type::Con(TyConId(1), vec![])
    }

    fn bool_ty() -> Type {
        Type::Con(TyConId(2), vec![])
    }

    fn fields(pairs: &[(&str, Type)]) -> Vec<(String, Type)> {
        pairs
            .iter()
            .map(|(n, t)| ((*n).to_string(), t.clone()))
            .collect()
    }

    // Order-insensitivity: {a: Int, b: Text} == {b: Text, a: Int}
    #[test]
    fn order_insensitive() {
        let k1 = shape_key(&fields(&[("a", int_ty()), ("b", text_ty())]));
        let k2 = shape_key(&fields(&[("b", text_ty()), ("a", int_ty())]));
        assert_eq!(k1, k2, "field order must not affect ShapeKey identity");
    }

    // Distinctness by field type: {a: Int} != {a: Text}
    #[test]
    fn distinct_by_field_type() {
        let k1 = shape_key(&fields(&[("a", int_ty())]));
        let k2 = shape_key(&fields(&[("a", text_ty())]));
        assert_ne!(k1, k2);
    }

    // Distinctness by field count: {a: Int} != {a: Int, b: Text}
    #[test]
    fn distinct_by_field_count() {
        let k1 = shape_key(&fields(&[("a", int_ty())]));
        let k2 = shape_key(&fields(&[("a", int_ty()), ("b", text_ty())]));
        assert_ne!(k1, k2);
    }

    // Empty record {}: ShapeKey(vec![]) distinct from {a: Int}
    #[test]
    fn empty_record_distinct() {
        let empty = shape_key(&[]);
        let nonempty = shape_key(&fields(&[("a", int_ty())]));
        assert_eq!(empty, ShapeKey(vec![]));
        assert_ne!(empty, nonempty);
    }

    // Alias equivalence: a field typed as Type::Alias { body: Int } should key
    // identically to a field typed as Int directly.
    #[test]
    fn alias_equivalence() {
        let int_direct = shape_key(&fields(&[("x", int_ty())]));
        // Simulate `type Age = Int`: alias wraps Int body.
        let alias_ty = Type::Alias {
            name: TyConId(99),
            body: Box::new(int_ty()),
        };
        let int_via_alias = shape_key(&fields(&[("x", alias_ty)]));
        assert_eq!(
            int_direct, int_via_alias,
            "alias-typed field must key identically to its resolved body"
        );
    }

    // Var type is preserved (not confused with other vars)
    #[test]
    fn var_distinctness() {
        let k1 = shape_key(&fields(&[("a", Type::Var(TyVid(0)))]));
        let k2 = shape_key(&fields(&[("a", Type::Var(TyVid(1)))]));
        let k3 = shape_key(&fields(&[("a", int_ty())]));
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
    }

    // Bool field in key round-trips correctly
    #[test]
    fn multiple_field_names_sort_ascending() {
        let k = shape_key(&fields(&[
            ("z", bool_ty()),
            ("a", int_ty()),
            ("m", text_ty()),
        ]));
        // The sorted key should have fields in order a, m, z
        let expected = ShapeKey(vec![
            ("a".to_string(), TyKey::Con(TyConId(0), vec![])),
            ("m".to_string(), TyKey::Con(TyConId(1), vec![])),
            ("z".to_string(), TyKey::Con(TyConId(2), vec![])),
        ]);
        assert_eq!(k, expected);
    }
}
