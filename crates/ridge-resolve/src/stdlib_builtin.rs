//! Hard-coded built-in stdlib manifest. Phase 7 will replace with a
//! generated table from the real stdlib (see R006).
//!
//! Export lists are derived from spec §9.1–9.2 (lines 1000–1037).
//! The table is provisional — Phase 7 generates it from the actual stdlib
//! source.  Adding a missing export here does NOT change language semantics.

// ── Public types ──────────────────────────────────────────────────────────────

/// Stable identifier for a built-in stdlib module.
///
/// The id value equals the index of the module in [`BUILTINS`]:
/// `BUILTINS[i].id.0 == i`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StdlibModuleId(pub u32);

/// A built-in stdlib module: its stable id, dot-separated name, and the list
/// of exported symbol names.
///
/// Export lists are **best-effort** from spec §9.1–9.2; Phase 7 supersedes
/// with a generated table from the real stdlib.
#[derive(Debug)]
pub struct BuiltinStdlibModule {
    /// Stable numeric id; equals the module's index in [`BUILTINS`].
    pub id: StdlibModuleId,
    /// Dot-separated module name, e.g. `"std.list"`.
    pub name: &'static str,
    /// Exported symbol names.  Slice is `&'static` for zero-cost lookup.
    pub exports: &'static [&'static str],
    /// Names of exported types declared `opaque`. Construction, pattern
    /// matching, and field access of these are confined to the stdlib module
    /// itself, so any use from user code is rejected (R025 / R026 / T036).
    pub opaque_types: &'static [&'static str],
}

/// The compile-time stdlib manifest.
///
/// Invariant: `BUILTINS[i].id.0 == i as u32` for all valid indices.
///
/// Ordering: matches `STDLIB_MODULE_ORDER` in
/// `crates/ridge-stdlib/src/codegen_manifest.rs` (tier-ordered); the
/// numeric id is what code should key on, not the name's sort position.
///
/// # Generated table
///
/// This slice is generated at build time by `crates/ridge-resolve/build.rs`,
/// which walks `../ridge-stdlib/stdlib/**/*.ridge` and extracts every `pub fn`
/// and `pub type` declaration.  The file
/// `${OUT_DIR}/stdlib_manifest.rs` (emitted by that build script) is
/// `include!`'d here.  The types used inside the generated file
/// (`BuiltinStdlibModule`, `StdlibModuleId`) are defined above in this same
/// module and are in scope at the `include!` site.
pub static BUILTINS: &[BuiltinStdlibModule] =
    include!(concat!(env!("OUT_DIR"), "/stdlib_manifest.rs"));

/// Look up a stdlib module by its dot-separated name (e.g. `"std.list"`).
///
/// Linear scan over [`BUILTINS`] — O(N) where N = 21.  Acceptable for the
/// current table size; Phase 7 may replace with a hash map if N grows.
#[must_use]
pub fn lookup_stdlib(name: &str) -> Option<&'static BuiltinStdlibModule> {
    BUILTINS.iter().find(|m| m.name == name)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Test 1: table length
    #[test]
    fn builtins_len_is_28() {
        assert_eq!(BUILTINS.len(), 28, "expected exactly 28 builtin modules");
    }

    // Test 2: each entry's id.0 == its index
    #[test]
    fn each_entry_id_equals_index() {
        for (i, entry) in BUILTINS.iter().enumerate() {
            assert_eq!(
                entry.id.0 as usize, i,
                "BUILTINS[{i}].id.0 must equal {i} but is {}",
                entry.id.0
            );
        }
    }

    // Test 3: lookup_stdlib("std.list") returns Some
    #[test]
    fn lookup_stdlib_std_list_is_some() {
        let m = lookup_stdlib("std.list");
        assert!(m.is_some(), "expected Some for 'std.list'");
        assert_eq!(m.unwrap().name, "std.list");
    }

    // Test 4: lookup_stdlib("std.bogus") returns None
    #[test]
    fn lookup_stdlib_std_bogus_is_none() {
        assert!(lookup_stdlib("std.bogus").is_none());
    }

    // Test 5: all example import paths resolve via lookup_stdlib
    #[test]
    fn all_example_stdlib_paths_resolve() {
        let paths = [
            "std.io",
            "std.fs",
            "std.env",
            "std.cli",
            "std.text",
            "std.list",
            "std.map",
            "std.option",
            "std.time",
            "std.random",
        ];
        for path in &paths {
            assert!(
                lookup_stdlib(path).is_some(),
                "expected Some for '{path}' but got None"
            );
        }
    }

    // Test 6: all 18 names are distinct
    #[test]
    fn all_builtin_names_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for entry in BUILTINS {
            let inserted = seen.insert(entry.name);
            assert!(inserted, "duplicate name: {}", entry.name);
        }
    }

    // Test 7: std.text exports include split, trim, lines, startsWith
    // (items used by the 4 canonical examples)
    #[test]
    fn std_text_exports_cover_example_items() {
        let m = lookup_stdlib("std.text").expect("std.text must exist");
        for item in &["split", "trim", "lines", "startsWith"] {
            assert!(m.exports.contains(item), "std.text must export '{item}'");
        }
    }

    // Test 8: std.map exports include get and insert
    #[test]
    fn std_map_exports_cover_expected_items() {
        let m = lookup_stdlib("std.map").expect("std.map must exist");
        for item in &["get", "insert"] {
            assert!(m.exports.contains(item), "std.map must export '{item}'");
        }
    }
}
