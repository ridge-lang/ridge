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
    ///
    /// Prefer [`Self::mentions`] to test membership and
    /// [`Self::dict_param_name`] to name a dictionary — both are correct for
    /// any arity. Reach for this only where the constraint is known to be
    /// single-parameter by construction.
    #[must_use]
    pub fn sole_ty(&self) -> TyVid {
        debug_assert_eq!(
            self.tys.len(),
            1,
            "sole_ty called on a multi-parameter constraint"
        );
        self.tys[0]
    }

    /// Does this constraint range over `v`?
    ///
    /// The membership test `sole_ty() == v` is really asking, and it asserts on
    /// anything with more than one variable. `Pairable a b` is reached through
    /// either of its variables.
    #[must_use]
    pub fn mentions(&self, v: TyVid) -> bool {
        self.tys.contains(&v)
    }

    /// The name of the dictionary parameter that carries this constraint.
    ///
    /// A single-parameter constraint keeps the historical spelling exactly —
    /// `$dict_ToText_3` — so nothing that already resolves moves. Beyond one
    /// variable the rest are appended, because the first does not identify the
    /// dictionary on its own: `Pairable a b` and `Pairable a c` are two
    /// different ones and would otherwise share a name.
    ///
    /// Every site that declares such a parameter and every site that
    /// references one goes through here, so the two cannot drift apart.
    #[must_use]
    pub fn dict_param_name(&self, class_name: &str) -> String {
        use std::fmt::Write as _;
        let mut out = format!("$dict_{class_name}");
        for v in &self.tys {
            let _ = write!(out, "_{}", v.0);
        }
        out
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
    fn mentions_finds_any_variable_not_just_the_first() {
        let c = Constraint::new(EQ_CLASS, smallvec![TyVid(1), TyVid(2)]);
        assert!(c.mentions(TyVid(1)));
        assert!(c.mentions(TyVid(2)), "the second variable reaches it too");
        assert!(!c.mentions(TyVid(3)));
    }

    /// A single-parameter dictionary name is byte-identical to the one this
    /// replaced, so nothing that already resolves moves.
    #[test]
    fn single_param_dict_name_is_unchanged() {
        let c = Constraint::single(TOTEXT_CLASS, TyVid(3));
        assert_eq!(c.dict_param_name("ToText"), "$dict_ToText_3");
    }

    /// Two constraints of the same class that share their first variable are
    /// different dictionaries and must not share a name. Naming from the first
    /// variable alone gave both `$dict_Pairable_1`.
    #[test]
    fn multi_param_dict_names_distinguish_the_rest_of_the_variables() {
        let ab = Constraint::new(EQ_CLASS, smallvec![TyVid(1), TyVid(2)]);
        let ac = Constraint::new(EQ_CLASS, smallvec![TyVid(1), TyVid(3)]);
        assert_eq!(ab.dict_param_name("Pairable"), "$dict_Pairable_1_2");
        assert_ne!(
            ab.dict_param_name("Pairable"),
            ac.dict_param_name("Pairable")
        );
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
