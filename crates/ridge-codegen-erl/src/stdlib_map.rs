//! §3.4 / §3.5 — Static stdlib bridge map (path A) and `BridgeTarget` enum.
//!
//! `lookup(module, name)` is the **only** call site in this crate that produces
//! BEAM module/function names from Ridge stdlib symbols.  After T11.5 the path-A
//! static map holds exactly six entries (`std.op.*`); all other stdlib symbols
//! are served by path B (`crate::ffi_targets::lookup` — the generated table).
//!
//! ## Arg order note
//!
//! `BeamStdlibPerm { perm }` is available for entries where Ridge surface
//! convention differs from BEAM arg order.  For `map`/`filter`/`forEach`, Phase 5
//! desugars pipe calls so the IR delivers `(fn, collection)` = BEAM order already.
//! Those entries therefore use `BeamStdlib` (no permutation) to avoid a
//! double-swap.  If Phase 5 ever delivers direct-call order `(collection, fn)` for
//! non-pipe invocations, revisit this.

#![allow(clippy::redundant_pub_crate)]

use rustc_hash::FxHashMap;
use std::sync::OnceLock;

// ── `BridgeTarget` — §3.5 verbatim ───────────────────────────────────────────

/// Codegen target for a Ridge stdlib symbol (§3.5).
///
/// `#[non_exhaustive]` so Phase 7 can add `RidgeStdlibLocal` without breaking
/// Phase 6 callers.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BridgeTarget {
    /// Path A: BEAM stdlib mod:fn/arity.  Phase 6 emission target.
    BeamStdlib {
        /// BEAM module atom (e.g. `"lists"`, `"erlang"`).
        module: &'static str,
        /// BEAM function name atom (e.g. `"map"`, `"length"`).
        fn_name: &'static str,
        /// Arity.
        arity: u32,
    },
    /// Path A with arg permutation: BEAM expects args in a different order.
    BeamStdlibPerm {
        /// BEAM module atom.
        module: &'static str,
        /// BEAM function name atom.
        fn_name: &'static str,
        /// Arity.
        arity: u32,
        /// `perm[i]` is the source-arg index for emitted-arg position `i`.
        ///
        /// Example: `perm = &[1, 0]` swaps a 2-arg call.
        perm: &'static [u32],
    },
    /// Path A wrapper in `ridge_rt.erl`: a hand-rolled adapter.
    RidgeRuntime {
        /// Function name in `ridge_rt` (e.g. `"list_head"`, `"println"`).
        fn_name: &'static str,
        /// Arity.
        arity: u32,
    },
    /// (Reserved for Phase 7) Compiled Ridge stdlib module.
    /// Variant gated behind `#[non_exhaustive]`; not emitted in 0.1.0.
    #[doc(hidden)]
    RidgeStdlibLocal {
        /// BEAM module produced by the Phase 7 stdlib compile.
        beam_module: String,
        /// Function name.
        fn_name: String,
        /// Arity.
        arity: u32,
    },
}

// ── Backing store ─────────────────────────────────────────────────────────────

type BridgeMap = FxHashMap<String, BridgeTarget>;

// The bridge-map table now contains only the six std.op.* entries (T11.5
// path-A retirement complete).  The allow attribute is retained for future additions.
#[allow(clippy::too_many_lines)]
fn build_map() -> BridgeMap {
    use BridgeTarget::BeamStdlib;

    // ── T11.5: path-A entries — final retirement ─────────────────────────────────
    //
    // T11 retired 29 @ffi-decorated entries to path B.  T11.5 retires the remaining
    // 15 cat-B/C entries (pure-Ridge bodies + name-change entries) by widening path
    // B to cover every `pub fn` in stdlib `.ridge` files.
    //
    // Only cat-A entries remain: `std.op.*` — emitted by `ridge-lower::operators`
    // with no Ridge surface; they have no `.ridge` body or `@ffi` annotation.
    // These six entries are permanent.
    //
    // Cat-B (pure-Ridge, no @ffi) retired in T11.5 (now served by path B):
    //   std.list.{head, drop, filterMap, find}
    //   std.map.{empty, get}
    //   std.option.{withDefault, flatMap}
    //   std.text.{split, startsWith, padLeft, lines, concat}
    //
    // Cat-C (name-change) retired in T11.5:
    //   std.env.var  → emit-site renamed to std.env.get  (path B serves "get")
    //   std.time.diffSeconds → emit-site renamed to std.time.diffMs (path B serves "diffMs")
    //
    // Effective count: 21 → 6.  Closes G3 (§11.2).

    let entries: &[(&'static str, &'static str, BridgeTarget)] = &[
        // ── std.op (polymorphic comparison operators) (cat A — permanent) ────
        // Emitted by ridge-lower::operators; no Ridge surface, no @ffi stub.
        // The plan uses "neq" but the lower phase emits "ne" (see operators.rs BinOp::Ne).
        (
            "std.op",
            "eq",
            BeamStdlib {
                module: "erlang",
                fn_name: "=:=",
                arity: 2,
            },
        ),
        (
            "std.op",
            "ne",
            BeamStdlib {
                module: "erlang",
                fn_name: "=/=",
                arity: 2,
            },
        ),
        (
            "std.op",
            "lt",
            BeamStdlib {
                module: "erlang",
                fn_name: "<",
                arity: 2,
            },
        ),
        (
            "std.op",
            "gt",
            BeamStdlib {
                module: "erlang",
                fn_name: ">",
                arity: 2,
            },
        ),
        (
            "std.op",
            "le",
            BeamStdlib {
                module: "erlang",
                fn_name: "=<",
                arity: 2,
            },
        ),
        (
            "std.op",
            "ge",
            BeamStdlib {
                module: "erlang",
                fn_name: ">=",
                arity: 2,
            },
        ),
    ];

    let mut map = FxHashMap::default();
    map.reserve(entries.len());
    for (module, name, target) in entries {
        // Key is "module::name" — double-colon avoids collisions with any single
        // dot-separated component that could theoretically contain a colon.
        let key = format!("{module}::{name}");
        map.insert(key, target.clone());
    }
    map
}

static BRIDGE_MAP: OnceLock<BridgeMap> = OnceLock::new();

// ── Seam adapter ──────────────────────────────────────────────────────────────
//
// Adapts `ridge_stdlib::ffi_targets::StdlibFfiTarget` (target-neutral) into
// `BridgeTarget::RidgeStdlibLocal` (BEAM-specific).  The adapter map is
// built once from `ridge_stdlib::ffi_targets::all_entries()` and cached in a
// `OnceLock`, mirroring the `BRIDGE_MAP` pattern.

fn build_stdlib_local_map() -> BridgeMap {
    let mut m = FxHashMap::default();
    for (key, t) in ridge_stdlib::ffi_targets::all_entries() {
        m.insert(
            key.to_owned(),
            BridgeTarget::RidgeStdlibLocal {
                beam_module: t.beam_module.clone(),
                fn_name: t.fn_name.clone(),
                arity: t.arity,
            },
        );
    }
    m
}

static STDLIB_LOCAL_MAP: OnceLock<BridgeMap> = OnceLock::new();

// ── Public API ────────────────────────────────────────────────────────────────

/// Look up the `BridgeTarget` for a Ridge stdlib symbol.
///
/// Returns `None` when no bridge entry exists — the caller should emit
/// `CodegenError::StdlibBridgeMissing` (E002).
///
/// ## Lookup strategy: path B then path A
///
/// Path B — consult the canonical Ridge stdlib FFI table first (covers both
/// `@ffi`-decorated stubs and pure-Ridge `pub fn` bodies).
/// `ridge_stdlib::ffi_targets::lookup` returns `Some(&'static StdlibFfiTarget)`;
/// the seam adapter converts this into `BridgeTarget::RidgeStdlibLocal` via
/// `STDLIB_LOCAL_MAP`.
///
/// Path A fallback — `BRIDGE_MAP` is the minimal kept set: exactly six
/// `std.op.*` entries (`eq, ne, lt, gt, le, ge`) emitted by
/// `ridge-lower::operators` with no Ridge surface.
#[must_use]
pub fn lookup(module: &str, name: &str) -> Option<&'static BridgeTarget> {
    // Path B — consult the canonical stdlib FFI table.
    if ridge_stdlib::ffi_targets::lookup(module, name).is_some() {
        let map: &'static BridgeMap = STDLIB_LOCAL_MAP.get_or_init(build_stdlib_local_map);
        let key = format!("{module}::{name}");
        if let Some(t) = map.get(&key) {
            return Some(t); // BridgeTarget::RidgeStdlibLocal
        }
    }
    // Path A fallback — the small kept set (std.op.*).
    let map: &'static BridgeMap = BRIDGE_MAP.get_or_init(build_map);
    let key = format!("{module}::{name}");
    map.get(&key)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── T11.5: G3 gate — build_map must contain exactly 6 entries ──────────────
    //
    // G3 (§11.2): path-A bridge map reduced to exactly `std.op.*` (6 entries).
    // All other stdlib symbols are served by path B (ffi_targets).
    #[test]
    fn build_map_count_is_exactly_6() {
        let map = build_map();
        assert_eq!(
            map.len(),
            6,
            "build_map must return exactly 6 entries (std.op.*) after T11.5; \
             got {}. G3 (§11.2) requires path-A retired to only std.op.*.",
            map.len()
        );
        // Verify all 6 are std.op.* entries.
        let op_names = ["eq", "ne", "lt", "gt", "le", "ge"];
        for name in op_names {
            assert!(
                map.contains_key(&format!("std.op::{name}")),
                "std.op.{name} must be in build_map"
            );
        }
    }

    #[test]
    fn lookup_unknown_returns_none() {
        assert!(lookup("std.unknown", "bogus").is_none());
        assert!(lookup("", "").is_none());
        assert!(lookup("std.list", "nonexistent").is_none());
    }

    // ── T11: path-B tests (std.list.map, std.io.println, std.int.toText) ────────
    //
    // After path-A retirement, these symbols are served by path B
    // (BridgeTarget::RidgeStdlibLocal) from the generated ffi_targets table.
    // The exact beam_module/fn_name/arity values are asserted to stay stable.

    #[test]
    fn lookup_list_map_is_stdlib_local() {
        // std.list.map is now served by path B: @ffi("lists", "map", 2) in list.ridge.
        match lookup("std.list", "map") {
            Some(BridgeTarget::RidgeStdlibLocal {
                beam_module,
                fn_name,
                arity,
            }) => {
                assert_eq!(beam_module, "lists");
                assert_eq!(fn_name, "map");
                assert_eq!(*arity, 2);
            }
            other => panic!("expected RidgeStdlibLocal for std.list.map, got {other:?}"),
        }
    }

    #[test]
    fn lookup_io_println_is_stdlib_local() {
        // std.io.println is now served by path B: @ffi("ridge_rt", "println", 1) in io.ridge.
        match lookup("std.io", "println") {
            Some(BridgeTarget::RidgeStdlibLocal {
                beam_module,
                fn_name,
                arity,
            }) => {
                assert_eq!(beam_module, "ridge_rt");
                assert_eq!(fn_name, "println");
                assert_eq!(*arity, 1);
            }
            other => panic!("expected RidgeStdlibLocal for std.io.println, got {other:?}"),
        }
    }

    #[test]
    fn lookup_int_to_text_is_stdlib_local() {
        // std.int.toText is now served by path B: @ffi("erlang", "integer_to_binary", 1).
        match lookup("std.int", "toText") {
            Some(BridgeTarget::RidgeStdlibLocal {
                beam_module,
                fn_name,
                arity,
            }) => {
                assert_eq!(beam_module, "erlang");
                assert_eq!(fn_name, "integer_to_binary");
                assert_eq!(*arity, 1);
            }
            other => panic!("expected RidgeStdlibLocal for std.int.toText, got {other:?}"),
        }
    }

    #[test]
    fn lookup_op_eq_is_erlang_op() {
        // std.op.eq is still path A (retained — emitted by ridge-lower::operators).
        match lookup("std.op", "eq") {
            Some(BridgeTarget::BeamStdlib {
                module,
                fn_name,
                arity,
            }) => {
                assert_eq!(*module, "erlang");
                assert_eq!(*fn_name, "=:=");
                assert_eq!(*arity, 2);
            }
            other => panic!("expected BeamStdlib for std.op.eq, got {other:?}"),
        }
    }

    #[test]
    fn lookup_all_ffi_example_symbols_have_entries() {
        // Sanity check: every @ffi-decorated symbol used by the four canonical
        // examples resolves through path B (RidgeStdlibLocal) or path A (std.op.*).
        //
        // Pure-Ridge functions (no @ffi) are NOT in this list — they lower to
        // ordinary Ridge calls and never appear as SymbolRef::Stdlib in the IR.
        // Examples of removed entries: std.option.withDefault, std.option.flatMap,
        // std.list.filterMap, std.list.find, std.list.head, std.list.drop,
        // std.list.range, std.map.empty, std.map.get, std.text.concat,
        // std.text.lines, std.text.startsWith, std.text.padLeft, std.text.split.
        let expected = &[
            // std.list — @ffi-decorated entries
            ("std.list", "map"),
            ("std.list", "fold"),
            ("std.list", "filter"),
            ("std.list", "forEach"),
            ("std.list", "length"),
            ("std.list", "sortBy"),
            ("std.list", "zip"),
            // std.map — @ffi-decorated entries
            ("std.map", "fromList"),
            ("std.map", "toList"),
            ("std.map", "insert"),
            // std.io — @ffi-decorated entries
            ("std.io", "println"),
            ("std.io", "print"),
            ("std.io", "eprintln"),
            // std.fs — @ffi-decorated entries
            ("std.fs", "lines"),
            // std.cli — @ffi-decorated entries
            ("std.cli", "args"),
            // std.time — @ffi-decorated entries
            ("std.time", "now"),
            ("std.time", "epoch"),
            ("std.time", "sleep"),
            // std.text — @ffi-decorated entries
            ("std.text", "trim"),
            ("std.text", "byteSize"),
            // std.int — @ffi-decorated entries
            ("std.int", "parse"),
            ("std.int", "toText"),
            ("std.int", "add"),
            ("std.int", "sub"),
            ("std.int", "mul"),
            ("std.int", "neg"),
            // std.float — @ffi-decorated entries
            ("std.float", "fromInt"),
            ("std.float", "toText"),
            // std.bool — @ffi-decorated entries
            ("std.bool", "not"),
            ("std.bool", "and"),
            ("std.bool", "or"),
            // std.random — @ffi-decorated entries
            ("std.random", "int"),
            ("std.random", "choice"),
            // std.net.http — @ffi-decorated entries
            ("std.net.http", "listen"),
            // std.op — retained path-A entries
            ("std.op", "eq"),
            ("std.op", "ne"),
            ("std.op", "lt"),
            ("std.op", "gt"),
            ("std.op", "le"),
            ("std.op", "ge"),
        ];
        for (module, name) in expected {
            assert!(
                lookup(module, name).is_some(),
                "missing bridge entry for {module}.{name}"
            );
        }
    }

    // ── T11.5: path-B cat-B coverage tests ───────────────────────────────────
    //
    // These pure-Ridge stdlib functions (formerly in path-A cat B) are now served
    // by path B with BridgeTarget::RidgeStdlibLocal where beam_module = ridge_module.

    #[test]
    fn lookup_list_head_is_stdlib_local_pure_ridge() {
        match lookup("std.list", "head") {
            Some(BridgeTarget::RidgeStdlibLocal {
                beam_module,
                fn_name,
                arity,
            }) => {
                assert_eq!(
                    beam_module, "std.list",
                    "pure-Ridge head: beam_module must be ridge module"
                );
                assert_eq!(fn_name, "head");
                assert_eq!(*arity, 1);
            }
            other => panic!("expected RidgeStdlibLocal(std.list:head/1), got {other:?}"),
        }
    }

    #[test]
    fn lookup_option_with_default_is_stdlib_local_pure_ridge() {
        match lookup("std.option", "withDefault") {
            Some(BridgeTarget::RidgeStdlibLocal {
                beam_module,
                fn_name,
                arity,
            }) => {
                assert_eq!(beam_module, "std.option");
                assert_eq!(fn_name, "withDefault");
                assert_eq!(*arity, 2);
            }
            other => panic!("expected RidgeStdlibLocal(std.option:withDefault/2), got {other:?}"),
        }
    }

    #[test]
    fn lookup_text_concat_is_stdlib_local_pure_ridge() {
        match lookup("std.text", "concat") {
            Some(BridgeTarget::RidgeStdlibLocal {
                beam_module,
                fn_name,
                arity,
            }) => {
                assert_eq!(beam_module, "std.text");
                assert_eq!(fn_name, "concat");
                assert_eq!(*arity, 2);
            }
            other => panic!("expected RidgeStdlibLocal(std.text:concat/2), got {other:?}"),
        }
    }

    #[test]
    fn lookup_env_var_returns_none_after_cat_c_retire() {
        // std.env.var was the old cat-C entry; the new API is std.env.get (served
        // by path B via @ffi).  After T11.5, "var" must not appear anywhere.
        assert!(
            lookup("std.env", "var").is_none(),
            "std.env.var must not be in any bridge after T11.5 cat-C retire"
        );
    }

    #[test]
    fn lookup_time_diff_seconds_returns_none_after_cat_c_retire() {
        // std.time.diffSeconds was the old cat-C entry; renamed to diffMs in
        // the example sources.  After T11.5 it must not appear in any bridge.
        assert!(
            lookup("std.time", "diffSeconds").is_none(),
            "std.time.diffSeconds must not be in any bridge after T11.5 cat-C retire"
        );
    }
}
