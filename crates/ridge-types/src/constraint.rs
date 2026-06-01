//! [`ClassId`] and [`Constraint`] — the fundamental class-system types.
//!
//! A [`Constraint`] asserts that a type variable satisfies a class (`C a`).
//! [`ClassId`] is an interned index into the workspace [`ClassTable`].
//!
//! These live in `ridge-types` (not `ridge-typecheck`) so that [`Scheme`] can
//! carry constraints without creating a dependency cycle: `ridge-types` has no
//! knowledge of the class registry; it only stores the interned id.

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

/// A single-parameter class constraint: `class_name type_var`.
///
/// Stored on [`crate::Scheme`] for polymorphic declarations that constrain
/// their type variables (e.g. `∀ a. ToText a => a -> Text`).
///
/// The constraint references a [`TyVid`] that is always one of the scheme's
/// `vars` — it is never a free inference variable in committed code.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Constraint {
    /// The class being required.
    pub class: ClassId,
    /// The constrained type variable (must appear in the enclosing
    /// [`crate::Scheme::vars`]).
    pub ty: TyVid,
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
        let a = Constraint {
            class: TOTEXT_CLASS,
            ty: TyVid(0),
        };
        let b = Constraint {
            class: TOTEXT_CLASS,
            ty: TyVid(0),
        };
        let c = Constraint {
            class: EQ_CLASS,
            ty: TyVid(0),
        };
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn constraint_clone() {
        let original = Constraint {
            class: ORD_CLASS,
            ty: TyVid(5),
        };
        let cloned = original.clone();
        assert_eq!(original, cloned);
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
