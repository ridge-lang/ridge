//! [`ClassId`] and [`Constraint`] — the fundamental class-system types.
//!
//! A [`Constraint`] asserts that a type variable satisfies a class (`C a`).
//! [`ClassId`] is an interned index into the workspace [`ClassTable`].
//!
//! These live in `ridge-types` (not `ridge-typecheck`) so that [`Scheme`] can
//! carry constraints without creating a dependency cycle: `ridge-types` has no
//! knowledge of the class registry; it only stores the interned id.

use smallvec::{smallvec, SmallVec};

use crate::ty::TyVid;

// ── ClassId ───────────────────────────────────────────────────────────────────

/// An interned class index, allocated by the workspace `ClassTable`.
///
/// Opaque to `ridge-types`; the name-to-id mapping lives in
/// `ridge-typecheck::class_env::ClassTable`.
///
/// Five fixed ids are reserved for the prelude classes:
/// - `0` — `ToText`
/// - `1` — `Eq`
/// - `2` — `Ord`
/// - `3` — `Encode`
/// - `4` — `Decode`
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ClassId(pub u32);

/// Reserved `ClassId` for the built-in `ToText` class.
pub const TOTEXT_CLASS: ClassId = ClassId(0);
/// Reserved `ClassId` for the built-in `Eq` class.
pub const EQ_CLASS: ClassId = ClassId(1);
/// Reserved `ClassId` for the built-in `Ord` class.
pub const ORD_CLASS: ClassId = ClassId(2);
/// Reserved `ClassId` for the built-in `Encode` class (`a -> JsonValue`).
pub const ENCODE_CLASS: ClassId = ClassId(3);
/// Reserved `ClassId` for the built-in `Decode` class (`JsonValue -> Result a Error`).
pub const DECODE_CLASS: ClassId = ClassId(4);

// ── Constraint ────────────────────────────────────────────────────────────────

/// A class constraint `class_name type_var…`.
///
/// Stored on [`crate::Scheme`] for polymorphic declarations that constrain
/// their type variables (e.g. `∀ a. ToText a => a -> Text`). The constrained
/// variables are held in `tys`: one for an ordinary single-parameter class,
/// several for a multi-parameter class such as `Convert a b`. The inline
/// length-1 backing means the overwhelmingly common single-parameter case
/// carries no heap allocation.
///
/// Each variable is always one of the scheme's `vars` — never a free
/// inference variable in committed code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Constraint {
    /// The class being required.
    pub class: ClassId,
    /// The constrained type variables (each must appear in the enclosing
    /// [`crate::Scheme::vars`]). Length one for a single-parameter class.
    pub tys: SmallVec<[TyVid; 1]>,
}

impl Constraint {
    /// Builds a single-parameter constraint `C a`.
    #[must_use]
    pub fn single(class: ClassId, ty: TyVid) -> Self {
        Self {
            class,
            tys: smallvec![ty],
        }
    }

    /// Builds a constraint over an explicit list of variables `C a b …`.
    #[must_use]
    pub const fn new(class: ClassId, tys: SmallVec<[TyVid; 1]>) -> Self {
        Self { class, tys }
    }

    /// Returns the sole constrained variable, for the single-parameter case.
    ///
    /// Debug builds assert the constraint really is single-parameter; this is
    /// the seam multi-parameter dispatch widens to walk every variable.
    #[must_use]
    pub fn sole_ty(&self) -> TyVid {
        debug_assert_eq!(
            self.tys.len(),
            1,
            "sole_ty called on a multi-parameter constraint"
        );
        self.tys[0]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_id_equality() {
        assert_eq!(TOTEXT_CLASS, ClassId(0));
        assert_ne!(TOTEXT_CLASS, EQ_CLASS);
        assert_ne!(EQ_CLASS, ORD_CLASS);
    }

    #[test]
    fn prelude_class_ids_are_distinct_and_sequential() {
        let ids = [
            TOTEXT_CLASS,
            EQ_CLASS,
            ORD_CLASS,
            ENCODE_CLASS,
            DECODE_CLASS,
        ];
        for (i, id) in ids.iter().enumerate() {
            assert_eq!(id.0 as usize, i, "prelude class ids must be 0..=4 in order");
        }
    }

    #[test]
    fn constraint_equality() {
        let a = Constraint::single(TOTEXT_CLASS, TyVid(0));
        let b = Constraint::single(TOTEXT_CLASS, TyVid(0));
        let c = Constraint::single(EQ_CLASS, TyVid(0));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn constraint_clone() {
        let original = Constraint::single(ORD_CLASS, TyVid(5));
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn sole_ty_returns_single_var() {
        let c = Constraint::single(EQ_CLASS, TyVid(7));
        assert_eq!(c.sole_ty(), TyVid(7));
        assert_eq!(c.tys.len(), 1);
    }

    #[test]
    fn multi_param_constraint_holds_every_var() {
        let c = Constraint::new(EQ_CLASS, smallvec![TyVid(1), TyVid(2)]);
        assert_eq!(c.tys.as_slice(), &[TyVid(1), TyVid(2)]);
    }

    #[test]
    fn class_id_hash() {
        use std::collections::HashSet;
        let mut s = HashSet::new();
        s.insert(TOTEXT_CLASS);
        s.insert(EQ_CLASS);
        s.insert(ORD_CLASS);
        assert_eq!(s.len(), 3);
    }
}
