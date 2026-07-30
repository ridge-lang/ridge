//! The ONE shape-hash function shared by every hot-reload consumer.
//!
//! A record's (or actor state's) runtime version identity is the 64-bit hash
//! of its canonical shape: the ordered list of `(field_name, rendered_type)`
//! pairs, where rendered types come from [`crate::render`] (FQN-stable, never
//! allocation-order `TyConId`s). `ridge-codegen-erl` hashes field layouts for
//! the `__ridge_v` value tag; `ridge-reload` hashes the same pairs into the
//! snapshot history. Because both call this function with the same inputs,
//! a tag baked into a beam and a hash stored in a snapshot can never diverge.

use rustc_hash::FxHasher;
use std::hash::Hasher;

/// Hash an ordered `(field_name, rendered_type)` shape into its 64-bit
/// version identity. Field order is significant (it is a *layout* hash).
#[must_use]
pub fn shape_hash(shape: &[(String, String)]) -> u64 {
    let mut h = FxHasher::default();
    for (name, ty) in shape {
        h.write(name.as_bytes());
        h.write(b":");
        h.write(ty.as_bytes());
        h.write(b";");
    }
    h.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(n, t)| ((*n).to_owned(), (*t).to_owned()))
            .collect()
    }

    #[test]
    fn same_shape_same_hash() {
        assert_eq!(
            shape_hash(&shape(&[("name", "Text"), ("age", "Int")])),
            shape_hash(&shape(&[("name", "Text"), ("age", "Int")]))
        );
    }

    #[test]
    fn hash_is_full_u64_width() {
        // The function returns u64; two shapes differing only in type must differ.
        let int_age = shape_hash(&shape(&[("age", "Int")]));
        let text_age = shape_hash(&shape(&[("age", "Text")]));
        assert_ne!(int_age, text_age);
        // A field rename changes the hash.
        let renamed = shape_hash(&shape(&[("years", "Int")]));
        assert_ne!(int_age, renamed);
        // Field ORDER matters (layout hash).
        let ordered_ab = shape_hash(&shape(&[("a", "Int"), ("b", "Int")]));
        let ordered_ba = shape_hash(&shape(&[("b", "Int"), ("a", "Int")]));
        assert_ne!(ordered_ab, ordered_ba);
    }
}
