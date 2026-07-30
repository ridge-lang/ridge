//! Version-history vocabulary shared by the typechecker, lowering, codegen,
//! and the reload snapshot tooling.
//!
//! Ordinals (`User@1`, `@version(N)`) are source-level sugar only; the runtime
//! identity is the 64-bit shape hash ([`crate::shape::shape_hash`]). The
//! history below is how the compiler resolves an ordinal to a hash: the
//! driver reads it out of the previous build's snapshot and injects it into
//! the pipeline, mirroring how it seeds beam target names.

use rustc_hash::FxHashMap;

/// One versioned shape of a record or of an actor's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionEntry {
    /// Source-level ordinal (`@version(N)` / the auto-assigned sequence).
    pub ordinal: u32,
    /// Runtime identity: [`crate::shape::shape_hash`] of `shape`.
    pub hash: u64,
    /// Ordered `(field_name, rendered_type)` pairs.
    pub shape: Vec<(String, String)>,
}

/// All known previous shapes of every record and actor state in a workspace.
///
/// Each list is oldest-first and — because it is built from a snapshot —
/// ends with the version that snapshot considered current. An empty history
/// is a fresh build: any `User@N` reference then fails with "no previous
/// version known".
#[derive(Debug, Clone, Default)]
pub struct VersionHistory {
    /// `(module_fqn, type_name)` → versions.
    pub records: FxHashMap<(String, String), Vec<VersionEntry>>,
    /// `(module_fqn, actor_name)` → versions.
    pub actors: FxHashMap<(String, String), Vec<VersionEntry>>,
}

impl VersionHistory {
    /// True when no record and no actor carries any history (fresh build).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty() && self.actors.is_empty()
    }

    /// The most recent entry for `ordinal` of record `name` in module `fqn`.
    /// Duplicate ordinals (a re-used `@version` override) resolve to the
    /// latest entry — first-win would resurrect a superseded shape.
    #[must_use]
    pub fn lookup_record(&self, fqn: &str, name: &str, ordinal: u32) -> Option<&VersionEntry> {
        self.records
            .get(&(fqn.to_owned(), name.to_owned()))
            .and_then(|entries| entries.iter().rev().find(|e| e.ordinal == ordinal))
    }

    /// The most recent entry for `ordinal` of actor `name`'s state in `fqn`.
    #[must_use]
    pub fn lookup_actor(&self, fqn: &str, name: &str, ordinal: u32) -> Option<&VersionEntry> {
        self.actors
            .get(&(fqn.to_owned(), name.to_owned()))
            .and_then(|entries| entries.iter().rev().find(|e| e.ordinal == ordinal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(ordinal: u32, hash: u64) -> VersionEntry {
        VersionEntry {
            ordinal,
            hash,
            shape: vec![("name".to_owned(), "Text".to_owned())],
        }
    }

    #[test]
    fn lookup_returns_last_entry_for_ordinal() {
        let mut h = VersionHistory::default();
        h.records.insert(
            ("app.m".to_owned(), "User".to_owned()),
            vec![entry(1, 111), entry(1, 222), entry(2, 333)],
        );
        // Duplicate ordinals (a re-used @version override) resolve to the
        // most recent entry.
        assert_eq!(
            h.lookup_record("app.m", "User", 1).map(|e| e.hash),
            Some(222)
        );
        assert_eq!(
            h.lookup_record("app.m", "User", 2).map(|e| e.hash),
            Some(333)
        );
        assert!(h.lookup_record("app.m", "User", 9).is_none());
        assert!(h.lookup_record("app.m", "Other", 1).is_none());
        assert!(h.lookup_actor("app.m", "User", 1).is_none());
        assert!(!h.is_empty());
        assert!(VersionHistory::default().is_empty());
    }
}
