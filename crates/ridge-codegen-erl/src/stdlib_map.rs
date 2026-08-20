//! §3.4 / §3.5 — Static stdlib bridge map (path A) and `BridgeTarget` enum.
//!
//! `lookup(module, name)` is the **only** call site in this crate that produces
//! BEAM module/function names from Ridge stdlib symbols.  The path-A static map
//! holds the symbols that name no implementation of their own — the comparison
//! operators, the slice-pattern helpers, and the arithmetic primitives — and
//! this crate is where each of them acquires a BEAM spelling.  Everything else
//! is served by path B (`crate::ffi_targets::lookup` — the generated table),
//! which describes declarations that named a target themselves.
//!
//! ## Why arithmetic is here
//!
//! `a + b` is a language operation, not a library call.  Its stdlib declaration
//! (`@primitive pub fn add …`) deliberately says nothing about how addition is
//! carried out, so the shared table has no entry for it and cannot hand a
//! second backend a BEAM module name for something as basic as `+`.  This file
//! is the whole of the BEAM's answer, and an LLVM backend writes a different
//! one without touching the stdlib or the IR.
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

// ── Arithmetic primitives ─────────────────────────────────────────────────────

/// How the BEAM spells each `@primitive` arithmetic symbol.
///
/// Kept as its own named table, separate from the entries folded into
/// [`build_map`], because two tests read it as a set: every symbol the stdlib
/// declares `@primitive` must appear here, and every symbol here must be one
/// the stdlib declares.  A one-way check would let either side grow an entry
/// the other never hears about — a primitive with no BEAM spelling fails at
/// codegen, and a spelling for a primitive nobody declares is dead weight that
/// reads like coverage.
///
/// Entries are `(ridge_module, ridge_name, beam_module, beam_fn, arity)`.
/// Adding one means editing this file, which is the intended friction: the set
/// of operations the language treats as primitive is a language decision, and
/// it should cost a deliberate edit in each backend that claims to implement
/// them.
pub(crate) const PRIMITIVE_ENTRIES: &[(&str, &str, &str, &str, u32)] = &[
    // Int — `div` and `rem` truncate toward zero, which is what the Ridge
    // declarations promise.
    ("std.int", "add", "erlang", "+", 2),
    ("std.int", "sub", "erlang", "-", 2),
    ("std.int", "mul", "erlang", "*", 2),
    ("std.int", "div", "erlang", "div", 2),
    ("std.int", "rem", "erlang", "rem", 2),
    ("std.int", "neg", "erlang", "-", 1),
    // Float — `/` rather than `div`, and the BEAM raises `badarith` on a zero
    // divisor rather than answering an infinity.
    ("std.float", "add", "erlang", "+", 2),
    ("std.float", "sub", "erlang", "-", 2),
    ("std.float", "mul", "erlang", "*", 2),
    ("std.float", "div", "erlang", "/", 2),
    ("std.float", "neg", "erlang", "-", 1),
];

// ── Backing store ─────────────────────────────────────────────────────────────

type BridgeMap = FxHashMap<String, BridgeTarget>;

/// Build the path-A table: the arithmetic primitives from
/// [`PRIMITIVE_ENTRIES`], plus the symbols below that have no Ridge surface at
/// all.
///
/// A stdlib function that declares its own target is *not* here — it is served
/// by the generated path-B table.  What lands in this file is everything whose
/// BEAM spelling is a decision this crate makes rather than one it reads.
#[allow(clippy::too_many_lines)]
fn build_map() -> BridgeMap {
    use BridgeTarget::BeamStdlib;

    let entries: &[(&'static str, &'static str, BridgeTarget)] = &[
        // ── std.op (polymorphic comparison operators) ─────────────────────────
        // Emitted by ridge-lower::operators; no Ridge surface, no declaration.
        // The lower phase emits "ne" for `!=` (see operators.rs BinOp::Ne).
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
        // ── __slice__ (list slice pattern helpers) ────────────────────────────
        // Emitted by ridge-lower when lowering suffix/middle list patterns.
        // These are internal IR symbols; they never appear in user-written Ridge code.
        (
            "__slice__",
            "length",
            BeamStdlib {
                module: "erlang",
                fn_name: "length",
                arity: 1,
            },
        ),
        (
            "__slice__",
            "hd",
            BeamStdlib {
                module: "erlang",
                fn_name: "hd",
                arity: 1,
            },
        ),
        (
            "__slice__",
            "tl",
            BeamStdlib {
                module: "erlang",
                fn_name: "tl",
                arity: 1,
            },
        ),
        (
            "__slice__",
            "ge",
            BeamStdlib {
                module: "erlang",
                fn_name: ">=",
                arity: 2,
            },
        ),
        (
            "__slice__",
            "nthtail",
            BeamStdlib {
                module: "lists",
                fn_name: "nthtail",
                arity: 2,
            },
        ),
        (
            "__slice__",
            "nth",
            BeamStdlib {
                module: "lists",
                fn_name: "nth",
                arity: 2,
            },
        ),
        (
            "__slice__",
            "sublist",
            BeamStdlib {
                module: "lists",
                fn_name: "sublist",
                arity: 3,
            },
        ),
        (
            "__slice__",
            "minus",
            BeamStdlib {
                module: "erlang",
                fn_name: "-",
                arity: 2,
            },
        ),
        (
            "__slice__",
            "and",
            BeamStdlib {
                module: "erlang",
                fn_name: "and",
                arity: 2,
            },
        ),
    ];

    let mut map = FxHashMap::default();
    map.reserve(entries.len() + PRIMITIVE_ENTRIES.len());
    for (module, name, beam_module, beam_fn, arity) in PRIMITIVE_ENTRIES {
        map.insert(
            format!("{module}::{name}"),
            BeamStdlib {
                module: beam_module,
                fn_name: beam_fn,
                arity: *arity,
            },
        );
    }
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
// Adapts `ridge_stdlib::stdlib_targets::StdlibTarget` (target-neutral) into
// `BridgeTarget` (BEAM-specific).  The adapter map is built once from
// `all_entries()` and cached in a `OnceLock`, mirroring `BRIDGE_MAP`.
//
// `StdlibTarget::Primitive` is deliberately absent from the adapter: it names
// no target to adapt, and `PRIMITIVE_ENTRIES` above is where the BEAM's answer
// for those symbols lives.

fn build_stdlib_local_map() -> BridgeMap {
    use ridge_stdlib::stdlib_targets::StdlibTarget;

    let mut m = FxHashMap::default();
    for (key, t) in ridge_stdlib::stdlib_targets::all_entries() {
        let target = match t {
            // A declaration that named a host function, and a compiled Ridge
            // body, reach the BEAM the same way: a module atom and a name.
            StdlibTarget::Foreign {
                module,
                fn_name,
                arity,
            }
            | StdlibTarget::RidgeModule {
                module,
                fn_name,
                arity,
            } => BridgeTarget::RidgeStdlibLocal {
                beam_module: module.clone(),
                fn_name: fn_name.clone(),
                arity: *arity,
            },
            StdlibTarget::Primitive { .. } => continue,
        };
        m.insert(key.to_owned(), target);
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
/// Path B — consult the shared stdlib table first.  It covers `@ffi` stubs and
/// pure-Ridge bodies, and the seam adapter converts either into
/// `BridgeTarget::RidgeStdlibLocal` via `STDLIB_LOCAL_MAP`.  A symbol the
/// stdlib declared `@primitive` is skipped here on purpose: it named nothing
/// to adapt, and answering from path B would route `1 + 2` through a call to
/// the stdlib wrapper instead of straight to the instruction.
///
/// Path A fallback — `BRIDGE_MAP` holds the symbols that name no
/// implementation of their own: the six `std.op.*` comparisons and the nine
/// `__slice__.*` helpers, both emitted by `ridge-lower` with no Ridge surface,
/// plus the eleven arithmetic primitives from [`PRIMITIVE_ENTRIES`], which do
/// have a Ridge surface but declare it `@primitive`.
#[must_use]
pub fn lookup(module: &str, name: &str) -> Option<&'static BridgeTarget> {
    let key = format!("{module}::{name}");

    // Path B — the shared stdlib table, for symbols that named a target.
    let shared = ridge_stdlib::stdlib_targets::lookup(module, name);
    if shared.is_some_and(|t| !t.is_primitive()) {
        let map: &'static BridgeMap = STDLIB_LOCAL_MAP.get_or_init(build_stdlib_local_map);
        if let Some(t) = map.get(&key) {
            return Some(t); // BridgeTarget::RidgeStdlibLocal
        }
    }

    // Path A — the symbols whose BEAM spelling this crate decides.
    let map: &'static BridgeMap = BRIDGE_MAP.get_or_init(build_map);
    map.get(&key)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // Path A holds three groups, and nothing else: the six `std.op.*`
    // comparisons and the nine `__slice__.*` helpers, neither of which has any
    // Ridge surface, plus the eleven arithmetic primitives, which do.
    #[test]
    fn build_map_has_std_op_slice_and_primitive_entries() {
        let map = build_map();
        let expected = 6 + 9 + PRIMITIVE_ENTRIES.len();
        assert_eq!(
            map.len(),
            expected,
            "build_map must return {expected} entries (6 std.op + 9 __slice__ + {} primitives); got {}.",
            PRIMITIVE_ENTRIES.len(),
            map.len()
        );
        // Verify all 6 std.op entries are present.
        let op_names = ["eq", "ne", "lt", "gt", "le", "ge"];
        for name in op_names {
            assert!(
                map.contains_key(&format!("std.op::{name}")),
                "std.op.{name} must be in build_map"
            );
        }
        // Verify all 9 __slice__ entries are present.
        let slice_names = [
            "length", "hd", "tl", "ge", "nthtail", "nth", "sublist", "minus", "and",
        ];
        for name in slice_names {
            assert!(
                map.contains_key(&format!("__slice__::{name}")),
                "__slice__.{name} must be in build_map"
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
            ("std.fs", "readDir"),
            ("std.fs", "isDir"),
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

    // ── The primitive set, checked in both directions ─────────────────────────
    //
    // The stdlib decides which operations are primitive; this crate decides how
    // the BEAM spells each one. Neither list is derived from the other, so the
    // only thing keeping them together is a check that reads both — and it has
    // to read both ways round. A one-directional check is never contradicted:
    // "every primitive has a spelling" stays true while this file grows an
    // entry for an operation nobody declares, and "every spelling is used"
    // stays true while the stdlib declares one this backend cannot emit.

    /// What the standard library declares `@primitive`, as `(module, name)`.
    fn declared_primitives() -> Vec<(String, String)> {
        ridge_stdlib::stdlib_targets::primitive_symbols()
            .into_iter()
            .map(|(m, n, _)| (m.to_owned(), n.to_owned()))
            .collect()
    }

    /// What this backend claims to be able to emit, as `(module, name)`.
    fn spelled_primitives() -> Vec<(String, String)> {
        let mut v: Vec<(String, String)> = PRIMITIVE_ENTRIES
            .iter()
            .map(|(m, n, _, _, _)| ((*m).to_owned(), (*n).to_owned()))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn every_declared_primitive_has_a_beam_spelling() {
        let declared = declared_primitives();
        assert!(
            !declared.is_empty(),
            "the stdlib declares no primitives at all — either the sources changed or the generated table stopped recognising `@primitive`"
        );
        let spelled = spelled_primitives();
        let missing: Vec<_> = declared.iter().filter(|d| !spelled.contains(d)).collect();
        assert!(
            missing.is_empty(),
            "the stdlib declares these `@primitive` symbols and this backend has no answer for them: {missing:?}"
        );
    }

    #[test]
    fn every_beam_spelling_answers_a_declared_primitive() {
        let declared = declared_primitives();
        let extra: Vec<_> = spelled_primitives()
            .into_iter()
            .filter(|s| !declared.contains(s))
            .collect();
        assert!(
            extra.is_empty(),
            "this backend spells these symbols as primitives but the stdlib declares no such thing: {extra:?}"
        );
    }

    #[test]
    fn a_primitive_resolves_to_the_instruction_not_to_the_stdlib_wrapper() {
        // The failure this guards against is silent and slow rather than loud:
        // if path B ever answers for a primitive, `1 + 2` becomes a call into
        // the compiled `std.int` module, which then calls `erlang:'+'`. It
        // still gives the right answer, one function call later, every time.
        match lookup("std.int", "add") {
            Some(BridgeTarget::BeamStdlib {
                module, fn_name, ..
            }) => {
                assert_eq!(*module, "erlang");
                assert_eq!(*fn_name, "+");
            }
            other => panic!("std.int.add must resolve to the BEAM instruction, got {other:?}"),
        }
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
