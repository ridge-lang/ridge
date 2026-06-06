//! [`CapabilitySet`] — a compact bit-set over the 10 Ridge capabilities.
//!
//! Bit layout (spec §3.5, D017, D035):
//! ```text
//! bit 0  = io
//! bit 1  = fs
//! bit 2  = net
//! bit 3  = time
//! bit 4  = random
//! bit 5  = env
//! bit 6  = proc
//! bit 7  = spawn
//! bit 8  = ffi
//! bit 9  = db
//! bits 10..15 = reserved (always zero — forward-extension slack under D035)
//! ```
//!
//! The set fits in a single `u16`; `CapabilitySet(0)` is the **pure** (empty)
//! set. The reserved high bits are never set by any public API.

use ridge_ast::Capability;

/// Bit-set over the 10 Ridge capabilities (spec §6.1, D017, D035).
///
/// Bit 0 = io, 1 = fs, 2 = net, 3 = time, 4 = random, 5 = env, 6 = proc,
/// 7 = spawn, 8 = ffi, 9 = db. Bits 10..15 are reserved (always zero), leaving
/// the set in a single `u16` with slack for forward extension under D035.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CapabilitySet(pub(crate) u16);

/// Bitmask covering only the 10 valid capability bits (0..=9).
const VALID_MASK: u16 = 0b0000_0011_1111_1111;

impl CapabilitySet {
    /// The pure (empty) capability set — no capabilities required.
    pub const PURE: Self = Self(0);

    /// Returns a set containing exactly one capability.
    #[must_use]
    pub const fn singleton(c: Capability) -> Self {
        Self(1u16 << bit_index(c))
    }

    /// Constructs a capability set from a raw `u16` bit-field.
    ///
    /// Only bits 0..=8 are meaningful; bit 9 is always cleared.
    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & VALID_MASK)
    }

    /// Returns the underlying `u16` bit-field.
    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Inserts a capability into the set (in-place).
    pub const fn insert(&mut self, c: Capability) {
        self.0 |= 1u16 << bit_index(c);
    }

    /// Removes a capability from the set (in-place).
    pub const fn remove(&mut self, c: Capability) {
        self.0 &= !(1u16 << bit_index(c));
    }

    /// Returns `true` if the set contains `c`.
    #[must_use]
    pub const fn contains(&self, c: Capability) -> bool {
        (self.0 >> bit_index(c)) & 1 == 1
    }

    /// Set union (join, `∪`): returns the set of capabilities in `self` OR `other`.
    #[must_use]
    pub const fn union(&self, other: &Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Set intersection (`∩`): returns capabilities in BOTH sets.
    #[must_use]
    pub const fn intersection(&self, other: &Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Set difference (`∖`): returns capabilities in `self` but NOT in `other`.
    #[must_use]
    pub const fn difference(&self, other: &Self) -> Self {
        Self(self.0 & !other.0)
    }

    /// Returns `true` if every capability in `self` is also in `of`
    /// (i.e., `self ⊆ of`).
    ///
    /// Used for the declared-vs-inferred check: `declared ⊇ inferred`.
    #[must_use]
    pub const fn is_subset(&self, of: &Self) -> bool {
        (self.0 & !of.0) == 0
    }

    /// Returns `true` if every capability in `other` is also in `self`
    /// (i.e., `self ⊇ other`).
    #[must_use]
    pub const fn is_superset(&self, of: &Self) -> bool {
        of.is_subset(self)
    }

    /// Returns `true` if the set is empty (pure — no capabilities).
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns `true` if the set is empty (alias for `is_empty` — named after
    /// the spec `pure` concept).
    #[must_use]
    pub const fn is_pure(self) -> bool {
        self.is_empty()
    }

    /// Returns the number of capabilities in the set.
    #[must_use]
    pub const fn len(self) -> u32 {
        self.0.count_ones()
    }

    /// Iterates over every [`Capability`] that is present in this set, in
    /// bit-index order (io, fs, net, time, random, env, proc, spawn, ffi).
    pub fn iter(self) -> impl Iterator<Item = Capability> {
        ALL_CAPABILITIES
            .iter()
            .copied()
            .filter(move |&c| self.contains(c))
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

/// Returns the bit index (0..=9) for the given capability.
const fn bit_index(c: Capability) -> u32 {
    match c {
        Capability::Io => 0,
        Capability::Fs => 1,
        Capability::Net => 2,
        Capability::Time => 3,
        Capability::Random => 4,
        Capability::Env => 5,
        Capability::Proc => 6,
        Capability::Spawn => 7,
        Capability::Ffi => 8,
        Capability::Db => 9,
    }
}

/// All 10 capabilities in bit-index order. Used by `iter()`.
const ALL_CAPABILITIES: [Capability; 10] = [
    Capability::Io,
    Capability::Fs,
    Capability::Net,
    Capability::Time,
    Capability::Random,
    Capability::Env,
    Capability::Proc,
    Capability::Spawn,
    Capability::Ffi,
    Capability::Db,
];

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Singleton & contains ──────────────────────────────────────────────────

    #[test]
    fn singleton_io_contains_io() {
        let s = CapabilitySet::singleton(Capability::Io);
        assert!(s.contains(Capability::Io));
    }

    #[test]
    fn singleton_io_does_not_contain_fs() {
        let s = CapabilitySet::singleton(Capability::Io);
        assert!(!s.contains(Capability::Fs));
    }

    #[test]
    fn singleton_ffi_bit_index_8() {
        let s = CapabilitySet::singleton(Capability::Ffi);
        assert_eq!(s.bits(), 1u16 << 8);
    }

    #[test]
    fn singleton_db_bit_index_9() {
        let s = CapabilitySet::singleton(Capability::Db);
        assert_eq!(s.bits(), 1u16 << 9);
    }

    #[test]
    fn pure_is_zero() {
        assert_eq!(CapabilitySet::PURE.bits(), 0);
    }

    #[test]
    fn all_singletons_are_distinct() {
        let sets: Vec<_> = ALL_CAPABILITIES
            .iter()
            .map(|&c| CapabilitySet::singleton(c))
            .collect();
        for i in 0..sets.len() {
            for j in 0..sets.len() {
                if i != j {
                    assert_ne!(sets[i], sets[j]);
                }
            }
        }
    }

    // ── insert / remove ───────────────────────────────────────────────────────

    #[test]
    fn insert_adds_capability() {
        let mut s = CapabilitySet::PURE;
        s.insert(Capability::Net);
        assert!(s.contains(Capability::Net));
        assert!(!s.contains(Capability::Io));
    }

    #[test]
    fn insert_idempotent() {
        let mut s = CapabilitySet::singleton(Capability::Io);
        s.insert(Capability::Io);
        assert_eq!(s, CapabilitySet::singleton(Capability::Io));
    }

    #[test]
    fn remove_clears_capability() {
        let mut s = CapabilitySet::singleton(Capability::Spawn);
        s.remove(Capability::Spawn);
        assert!(!s.contains(Capability::Spawn));
        assert!(s.is_empty());
    }

    #[test]
    fn remove_absent_is_noop() {
        let mut s = CapabilitySet::singleton(Capability::Io);
        s.remove(Capability::Fs);
        assert!(s.contains(Capability::Io));
    }

    // ── union ─────────────────────────────────────────────────────────────────

    #[test]
    fn union_combines_caps() {
        let a = CapabilitySet::singleton(Capability::Io);
        let b = CapabilitySet::singleton(Capability::Fs);
        let u = a.union(&b);
        assert!(u.contains(Capability::Io));
        assert!(u.contains(Capability::Fs));
        assert!(!u.contains(Capability::Net));
    }

    #[test]
    fn union_with_pure_is_identity() {
        let s = CapabilitySet::singleton(Capability::Time);
        let u = s.union(&CapabilitySet::PURE);
        assert_eq!(u, s);
    }

    #[test]
    fn union_is_commutative() {
        let a = CapabilitySet::singleton(Capability::Io);
        let b = CapabilitySet::singleton(Capability::Net);
        assert_eq!(a.union(&b), b.union(&a));
    }

    // ── intersection ──────────────────────────────────────────────────────────

    #[test]
    fn intersection_of_disjoint_is_empty() {
        let a = CapabilitySet::singleton(Capability::Io);
        let b = CapabilitySet::singleton(Capability::Fs);
        assert!(a.intersection(&b).is_empty());
    }

    #[test]
    fn intersection_of_overlapping() {
        let a = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));
        let b = CapabilitySet::singleton(Capability::Fs)
            .union(&CapabilitySet::singleton(Capability::Net));
        let i = a.intersection(&b);
        assert!(i.contains(Capability::Fs));
        assert!(!i.contains(Capability::Io));
        assert!(!i.contains(Capability::Net));
    }

    // ── difference ────────────────────────────────────────────────────────────

    #[test]
    fn difference_removes_shared_caps() {
        let a = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));
        let b = CapabilitySet::singleton(Capability::Fs);
        let d = a.difference(&b);
        assert!(d.contains(Capability::Io));
        assert!(!d.contains(Capability::Fs));
    }

    #[test]
    fn difference_with_pure_is_identity() {
        let s = CapabilitySet::singleton(Capability::Env);
        assert_eq!(s.difference(&CapabilitySet::PURE), s);
    }

    // ── subset / superset ─────────────────────────────────────────────────────

    #[test]
    fn pure_is_subset_of_everything() {
        let s = CapabilitySet::singleton(Capability::Ffi);
        assert!(CapabilitySet::PURE.is_subset(&s));
    }

    #[test]
    fn everything_is_superset_of_pure() {
        let s = CapabilitySet::singleton(Capability::Random);
        assert!(s.is_superset(&CapabilitySet::PURE));
    }

    #[test]
    fn subset_false_when_extra_caps() {
        let declared = CapabilitySet::singleton(Capability::Io);
        let inferred = CapabilitySet::singleton(Capability::Io)
            .union(&CapabilitySet::singleton(Capability::Fs));
        // declared ⊆ inferred is true; inferred ⊆ declared is false
        assert!(declared.is_subset(&inferred));
        assert!(!inferred.is_subset(&declared));
    }

    #[test]
    fn superset_reflexive() {
        let s = CapabilitySet::singleton(Capability::Net);
        assert!(s.is_superset(&s));
    }

    // ── is_empty / is_pure / len ──────────────────────────────────────────────

    #[test]
    fn pure_is_empty() {
        assert!(CapabilitySet::PURE.is_empty());
        assert!(CapabilitySet::PURE.is_pure());
    }

    #[test]
    fn singleton_is_not_empty() {
        assert!(!CapabilitySet::singleton(Capability::Io).is_empty());
    }

    #[test]
    fn len_pure_is_zero() {
        assert_eq!(CapabilitySet::PURE.len(), 0);
    }

    #[test]
    fn len_singleton_is_one() {
        assert_eq!(CapabilitySet::singleton(Capability::Spawn).len(), 1);
    }

    #[test]
    fn len_all_caps_is_ten() {
        let mut all = CapabilitySet::PURE;
        for &c in &ALL_CAPABILITIES {
            all.insert(c);
        }
        assert_eq!(all.len(), 10);
    }

    // ── iter ──────────────────────────────────────────────────────────────────

    #[test]
    fn iter_pure_is_empty() {
        assert_eq!(CapabilitySet::PURE.iter().count(), 0);
    }

    #[test]
    fn iter_singleton_yields_one() {
        let s = CapabilitySet::singleton(Capability::Time);
        let v: Vec<_> = s.iter().collect();
        assert_eq!(v, vec![Capability::Time]);
    }

    #[test]
    fn iter_all_caps_yields_ten_in_order() {
        let mut all = CapabilitySet::PURE;
        for &c in &ALL_CAPABILITIES {
            all.insert(c);
        }
        let v: Vec<_> = all.iter().collect();
        assert_eq!(v, ALL_CAPABILITIES.to_vec());
    }

    // ── from_bits / bits ──────────────────────────────────────────────────────

    #[test]
    fn from_bits_clears_reserved_high_bits() {
        // bits 10..15 are out of range and must be masked away; bit 9 (db) stays.
        let s = CapabilitySet::from_bits(1u16 << 10 | 1u16 << 9 | 1u16 << 0);
        assert_eq!(s.bits(), 1u16 << 9 | 1u16);
    }

    #[test]
    fn reserved_bits_never_set_by_insert() {
        let mut s = CapabilitySet::PURE;
        for &c in &ALL_CAPABILITIES {
            s.insert(c);
        }
        // bits 10..15 must remain zero (only 0..=9 are real capabilities).
        assert_eq!(s.bits() & !VALID_MASK, 0);
    }
}
