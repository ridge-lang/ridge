//! [`BuiltinTyCons`] — the table of built-in type-constructor identifiers.
//!
//! # T3 implementation
//!
//! `BuiltinTyCons::allocate(&mut TyConArena)` registers the 12 built-in
//! `TyCons` (indices 0..11) and returns populated struct fields.
//!
//! Calling `unallocated()` is still available for tests and scaffolding that
//! hasn't wired the real arena yet.

use crate::{
    capability_set::CapabilitySet,
    ty::{TyVid, Type},
    tycon::{
        RecordField, RecordSchema, TyConArena, TyConDecl, TyConId, TyConKind, UnionSchema,
        UnionVariant, VariantPayload,
    },
};

/// Built-in `TyCon` ids — assigned at workspace-init time, then immutable.
///
/// `#[non_exhaustive]` so that adding a new built-in (e.g. in 0.2.0) is
/// non-breaking for downstream match sites.
#[non_exhaustive]
#[derive(Debug)]
pub struct BuiltinTyCons {
    /// `Int` — 64-bit signed integer (D029).
    pub int: TyConId,
    /// `Float` — IEEE-754 double-precision float.
    pub float: TyConId,
    /// `Bool` — boolean.
    pub bool: TyConId,
    /// `Text` — UTF-8 string.
    pub text: TyConId,
    /// `Unit` — the unit type `()`.
    pub unit: TyConId,
    /// `Timestamp` — wall-clock time (D048).
    pub timestamp: TyConId,
    /// `List a` — an ordered sequence.
    pub list: TyConId,
    /// `Map k v` — an ordered key-value mapping (ordered/deterministic).
    pub map: TyConId,
    /// `Set a` — an ordered set.
    pub set: TyConId,
    /// `Option a` — an optional value.
    pub option: TyConId,
    /// `Result a e` — a fallible computation.
    pub result: TyConId,
    /// `Handle a` — a reference to a running actor instance (D061).
    pub handle: TyConId,
    /// `Error { code: Text, message: Text }` — stdlib error record (§3.11, OQ-S007).
    ///
    /// Used as the `e` parameter of `Result _ Error` returns in `std.io`,
    /// `std.fs`, `std.time`, `std.proc`.  Registered as a `TyConKind::Record`
    /// so that field access (`err.code`, `err.message`) is typeable.
    pub error: TyConId,
    /// `Duration { ms: Int }` — time difference record (§3.12).
    ///
    /// Returned by `std.time.diff`.
    pub duration: TyConId,
    /// `ProcOutput { stdout: Text, stderr: Text, exitCode: Int }` — process
    /// output record (§3.16 / OQ-S007 / D123).
    ///
    /// Returned as the `Ok` payload of `std.proc.run`.
    pub proc_output: TyConId,
    /// `Ordering = Less | Equal | Greater` — the result type of `compare`.
    ///
    /// Required by the `Ord` typeclass (0.2.13). Registered as a prelude
    /// union type so any module can match on `Less`, `Equal`, `Greater`
    /// without an explicit import.
    pub ordering: TyConId,
}

impl BuiltinTyCons {
    /// Returns an uninitialised `BuiltinTyCons` with sentinel values.
    ///
    /// **Panics** if any field is used before `allocate` (T3) has been called.
    /// This constructor exists only so that T2 types compile; real allocation
    /// is implemented in T3.
    #[must_use]
    pub const fn unallocated() -> Self {
        // Sentinel value — any use before T3 wires the real IDs will panic at
        // the call site (via the debug assertion in T3's allocator).
        const SENTINEL: TyConId = TyConId(u32::MAX);
        Self {
            int: SENTINEL,
            float: SENTINEL,
            bool: SENTINEL,
            text: SENTINEL,
            unit: SENTINEL,
            timestamp: SENTINEL,
            list: SENTINEL,
            map: SENTINEL,
            set: SENTINEL,
            option: SENTINEL,
            result: SENTINEL,
            handle: SENTINEL,
            error: SENTINEL,
            duration: SENTINEL,
            proc_output: SENTINEL,
            ordering: SENTINEL,
        }
    }

    /// Allocates the 15 built-in `TyCons` into `arena` and returns a populated
    /// `BuiltinTyCons`.
    ///
    /// Indices are assigned in a fixed order (Int=0, Float=1, Bool=2, Text=3,
    /// Unit=4, Timestamp=5, List=6, Map=7, Set=8, Option=9, Result=10,
    /// Handle=11, Error=12, Duration=13, ProcOutput=14) matching spec §4.1.
    /// Callers must pass a **fresh** arena (i.e. `arena.is_empty()` must be
    /// true) so that the resulting `TyConId`s are stable and predictable.
    ///
    /// # Panics
    ///
    /// Panics (debug only) if `arena` is not empty — indicates caller error:
    /// built-ins must be the first entries in the arena.
    #[must_use]
    #[expect(
        clippy::too_many_lines,
        reason = "flat sequential arena.intern() calls; splitting would harm readability without reducing complexity"
    )]
    #[allow(
        clippy::cognitive_complexity,
        reason = "same as too_many_lines above — clippy 1.88 also flags cognitive_complexity here"
    )]
    pub fn allocate(arena: &mut TyConArena) -> Self {
        debug_assert!(
            arena.is_empty(),
            "BuiltinTyCons::allocate requires an empty arena; got {} entries",
            arena.len()
        );

        // ── Primitive atom types (arity 0, TyConKind::Primitive) ──────────────
        let int = arena.intern(TyConDecl {
            id: TyConId(0), // overwritten by arena.intern
            name: "Int".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let float = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Float".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let bool_ = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Bool".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let text = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Text".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let unit = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Unit".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let timestamp = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Timestamp".to_string(),
            arity: 0,
            kind: TyConKind::Primitive,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });

        // ── Generic built-in containers (TyConKind::Builtin) ──────────────────
        let list = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "List".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let map = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Map".to_string(),
            arity: 2,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        let set = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Set".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });

        // ── Prelude unions (TyConKind::Union) ─────────────────────────────────
        //
        // Option and Result carry canonical UnionSchemas so that T4 can attach
        // the right Scheme to `Some`, `None`, `Ok`, `Err` (§4.3).
        // The type-variable TyVids used here are *schema-level* placeholders,
        // not inference variables; they are stable dummy IDs that the prelude
        // wiring (T4) will replace with fresh ones on each instantiation.
        let option = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Option".to_string(),
            arity: 1,
            kind: TyConKind::Union(UnionSchema {
                params: vec![TyVid(0)],
                variants: vec![
                    UnionVariant {
                        name: "Some".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                    },
                    UnionVariant {
                        name: "None".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        // Result a e — Ok a | Err e  (spec: Result a e)
        let result = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Result".to_string(),
            arity: 2,
            kind: TyConKind::Union(UnionSchema {
                params: vec![TyVid(0), TyVid(1)],
                variants: vec![
                    UnionVariant {
                        name: "Ok".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Var(TyVid(0))]),
                    },
                    UnionVariant {
                        name: "Err".to_string(),
                        kind: VariantPayload::Positional(vec![Type::Var(TyVid(1))]),
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });

        // ── Handle a — phantom actor-reference type (TyConKind::Builtin) ──────
        //
        // Handle is a 1-arity opaque type; its "schema" is the actor's TyConDecl
        // (looked up at use sites).  D061: `spawn ActorName args` produces a
        // `Handle(ActorTyCon)`.
        let handle = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Handle".to_string(),
            arity: 1,
            kind: TyConKind::Builtin,
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });

        // ── Stdlib record types (TyConKind::Record) ───────────────────────────
        //
        // These are declared as `TyConKind::Record` so that field access is
        // typeable (e.g. `err.code : Text`).  They parallel `Timestamp` in that
        // they are pre-allocated in `BuiltinTyCons` rather than arising from a
        // user `TypeDecl` — the stdlib build pipeline compiles each tier in
        // isolation, so cross-tier record references must be pre-registered here.
        //
        // The `Text` and `Int` field types reference the `text` and `int` ids
        // allocated above; at this point in `allocate` those ids are valid.
        //
        // §3.11 / OQ-S007: Error { code: Text, message: Text }
        let error = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Error".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "code".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "message".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        // §3.12: Duration { ms: Int }
        let duration = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Duration".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![RecordField {
                    name: "ms".to_string(),
                    ty: Type::Con(int, vec![]),
                }],
            )),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });
        // §3.16 / OQ-S007 / D123: ProcOutput { stdout: Text, stderr: Text, exitCode: Int }
        let proc_output = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "ProcOutput".to_string(),
            arity: 0,
            kind: TyConKind::Record(RecordSchema::new(
                vec![],
                vec![
                    RecordField {
                        name: "stdout".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "stderr".to_string(),
                        ty: Type::Con(text, vec![]),
                    },
                    RecordField {
                        name: "exitCode".to_string(),
                        ty: Type::Con(int, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            is_anon: false,
        });

        // Ordering = Less | Equal | Greater (0.2.13 prelude type, required by Ord)
        let ordering = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Ordering".to_string(),
            arity: 0,
            kind: TyConKind::Union(UnionSchema {
                params: vec![],
                variants: vec![
                    UnionVariant {
                        name: "Less".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Equal".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                    UnionVariant {
                        name: "Greater".to_string(),
                        kind: VariantPayload::Nullary,
                    },
                ],
            }),
            def_span: None,
            def_module_raw: None, // prelude — no user module
            is_anon: false,
        });

        // Verify assignment order matches spec §4.1 indices 0..15.
        debug_assert_eq!(int.0, 0);
        debug_assert_eq!(float.0, 1);
        debug_assert_eq!(bool_.0, 2);
        debug_assert_eq!(text.0, 3);
        debug_assert_eq!(unit.0, 4);
        debug_assert_eq!(timestamp.0, 5);
        debug_assert_eq!(list.0, 6);
        debug_assert_eq!(map.0, 7);
        debug_assert_eq!(set.0, 8);
        debug_assert_eq!(option.0, 9);
        debug_assert_eq!(result.0, 10);
        debug_assert_eq!(handle.0, 11);
        debug_assert_eq!(error.0, 12);
        debug_assert_eq!(duration.0, 13);
        debug_assert_eq!(proc_output.0, 14);
        debug_assert_eq!(ordering.0, 15);

        // Suppress the "unused" lint — CapabilitySet is imported for future use
        // in T4 (actor schemas carry CapabilitySet).
        let _ = CapabilitySet::PURE;

        Self {
            int,
            float,
            bool: bool_,
            text,
            unit,
            timestamp,
            list,
            map,
            set,
            option,
            result,
            handle,
            error,
            duration,
            proc_output,
            ordering,
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_arena_with_builtins() -> (TyConArena, BuiltinTyCons) {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        (arena, b)
    }

    // ── TyConId uniqueness ────────────────────────────────────────────────────

    #[test]
    fn fifteen_distinct_ids() {
        let (_, b) = make_arena_with_builtins();
        let ids = [
            b.int,
            b.float,
            b.bool,
            b.text,
            b.unit,
            b.timestamp,
            b.list,
            b.map,
            b.set,
            b.option,
            b.result,
            b.handle,
            b.error,
            b.duration,
            b.proc_output,
        ];
        // All 15 ids must be distinct.
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(id.0), "duplicate TyConId: {}", id.0);
        }
        assert_eq!(seen.len(), 15);
    }

    #[test]
    fn ids_match_spec_order() {
        let (_, b) = make_arena_with_builtins();
        assert_eq!(b.int.0, 0);
        assert_eq!(b.float.0, 1);
        assert_eq!(b.bool.0, 2);
        assert_eq!(b.text.0, 3);
        assert_eq!(b.unit.0, 4);
        assert_eq!(b.timestamp.0, 5);
        assert_eq!(b.list.0, 6);
        assert_eq!(b.map.0, 7);
        assert_eq!(b.set.0, 8);
        assert_eq!(b.option.0, 9);
        assert_eq!(b.result.0, 10);
        assert_eq!(b.handle.0, 11);
        assert_eq!(b.error.0, 12);
        assert_eq!(b.duration.0, 13);
        assert_eq!(b.proc_output.0, 14);
    }

    #[test]
    fn int_ne_float() {
        let (_, b) = make_arena_with_builtins();
        assert_ne!(b.int, b.float);
    }

    #[test]
    fn list_ne_option() {
        let (_, b) = make_arena_with_builtins();
        assert_ne!(b.list, b.option);
    }

    #[test]
    fn arena_len_is_16() {
        // 15 original builtins + Ordering (added in 0.2.13 for the Ord typeclass)
        let (arena, _) = make_arena_with_builtins();
        assert_eq!(arena.len(), 16);
    }

    // ── Arena get() round-trip ────────────────────────────────────────────────

    #[test]
    fn arena_get_int_name() {
        let (arena, b) = make_arena_with_builtins();
        assert_eq!(arena.get(b.int).name, "Int");
    }

    #[test]
    fn arena_get_option_is_union() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.option);
        assert!(matches!(decl.kind, TyConKind::Union(_)));
        assert_eq!(decl.arity, 1);
    }

    #[test]
    fn arena_get_result_is_union() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.result);
        assert!(matches!(decl.kind, TyConKind::Union(_)));
        assert_eq!(decl.arity, 2);
    }

    #[test]
    fn arena_get_list_is_builtin() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.list);
        assert!(matches!(decl.kind, TyConKind::Builtin));
        assert_eq!(decl.arity, 1);
    }

    #[test]
    fn arena_get_map_is_builtin_arity_2() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.map);
        assert!(matches!(decl.kind, TyConKind::Builtin));
        assert_eq!(decl.arity, 2);
    }

    #[test]
    fn arena_get_handle_is_builtin_arity_1() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.handle);
        assert!(matches!(decl.kind, TyConKind::Builtin));
        assert_eq!(decl.arity, 1);
    }

    #[test]
    fn option_schema_has_some_and_none() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.option);
        if let TyConKind::Union(schema) = &decl.kind {
            assert_eq!(schema.variants.len(), 2);
            assert_eq!(schema.variants[0].name, "Some");
            assert_eq!(schema.variants[1].name, "None");
        } else {
            panic!("Option must be a Union TyCon");
        }
    }

    #[test]
    fn result_schema_has_ok_and_err() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.result);
        if let TyConKind::Union(schema) = &decl.kind {
            assert_eq!(schema.variants.len(), 2);
            assert_eq!(schema.variants[0].name, "Ok");
            assert_eq!(schema.variants[1].name, "Err");
        } else {
            panic!("Result must be a Union TyCon");
        }
    }

    #[test]
    fn error_schema_has_code_and_message() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.error);
        assert_eq!(decl.name, "Error");
        assert_eq!(decl.arity, 0);
        if let TyConKind::Record(schema) = &decl.kind {
            let fields = schema.record_fields();
            assert_eq!(fields.len(), 2, "Error must have 2 fields");
            assert_eq!(fields[0].name, "code");
            assert_eq!(fields[1].name, "message");
            assert!(
                matches!(fields[0].ty, Type::Con(id, _) if id == b.text),
                "code must be Text"
            );
            assert!(
                matches!(fields[1].ty, Type::Con(id, _) if id == b.text),
                "message must be Text"
            );
        } else {
            panic!("Error must be a Record TyCon");
        }
    }

    #[test]
    fn duration_schema_has_ms() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.duration);
        assert_eq!(decl.name, "Duration");
        assert_eq!(decl.arity, 0);
        if let TyConKind::Record(schema) = &decl.kind {
            let fields = schema.record_fields();
            assert_eq!(fields.len(), 1, "Duration must have 1 field");
            assert_eq!(fields[0].name, "ms");
            assert!(
                matches!(fields[0].ty, Type::Con(id, _) if id == b.int),
                "ms must be Int"
            );
        } else {
            panic!("Duration must be a Record TyCon");
        }
    }

    #[test]
    fn proc_output_schema_has_stdout_stderr_exit_code() {
        let (arena, b) = make_arena_with_builtins();
        let decl = arena.get(b.proc_output);
        assert_eq!(decl.name, "ProcOutput");
        assert_eq!(decl.arity, 0);
        if let TyConKind::Record(schema) = &decl.kind {
            let fields = schema.record_fields();
            assert_eq!(fields.len(), 3, "ProcOutput must have 3 fields");
            assert_eq!(fields[0].name, "stdout");
            assert_eq!(fields[1].name, "stderr");
            assert_eq!(fields[2].name, "exitCode");
            assert!(
                matches!(fields[0].ty, Type::Con(id, _) if id == b.text),
                "stdout must be Text"
            );
            assert!(
                matches!(fields[1].ty, Type::Con(id, _) if id == b.text),
                "stderr must be Text"
            );
            assert!(
                matches!(fields[2].ty, Type::Con(id, _) if id == b.int),
                "exitCode must be Int"
            );
        } else {
            panic!("ProcOutput must be a Record TyCon");
        }
    }

    #[test]
    fn primitives_have_arity_zero() {
        let (arena, b) = make_arena_with_builtins();
        for id in [
            b.int,
            b.float,
            b.bool,
            b.text,
            b.unit,
            b.timestamp,
            b.error,
            b.duration,
            b.proc_output,
        ] {
            let decl = arena.get(id);
            assert_eq!(decl.arity, 0, "{} must have arity 0", decl.name);
        }
    }

    #[test]
    fn all_def_spans_are_none() {
        let (arena, _) = make_arena_with_builtins();
        for decl in arena.all() {
            assert!(
                decl.def_span.is_none(),
                "{} must have no def_span (built-in)",
                decl.name
            );
        }
    }
}
