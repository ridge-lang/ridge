//! String-interpolation typing: `ToText` instance-registry dispatch.
//!
//! # Spec reference: §4.11 / D038
//!
//! Each `${expr}` hole in an interpolated string must resolve to a type that
//! has a `ToText` instance in the workspace instance registry. The prelude
//! registers built-in instances for `Int`, `Float`, `Bool`, `Text`,
//! `Timestamp`, and `Ordering`; user-defined types acquire an instance either
//! by writing `instance ToText T` or by declaring `pub fn toText (x: T) ->
//! Text` (auto-promoted during the collect pass).
//!
//! When no `ToText` instance exists for the hole's type, the diagnostic is
//! **T029 `NoInstance`** (not T012, which is retired in this cut).
//!
//! # Absorbing rule
//!
//! If a sub-expression already resolved to `Type::Error` (meaning an earlier
//! `T###` was already emitted for it), this pass is silent — no error is
//! stacked on top.
//!
//! # Free type variables
//!
//! When the hole type is a free `Type::Var`, no error is emitted here.  In
//! correct programs this arises in constrained polymorphic functions (e.g.
//! `fn describe (x: a) -> Text where ToText a`); the constraint solver
//! already verified that `a` has a `ToText` instance at every concrete call
//! site.  An unresolved var that truly lacks a `ToText` constraint will
//! produce a T023 or T029 from the constraint solver.

use ridge_ast::{InterpPart, Span};
use ridge_types::{BuiltinTyCons, TyConId, Type};
use rustc_hash::FxHashSet;

use crate::ctx::InferCtx;
use crate::error::TypeError;
use crate::infer::infer_expr;

// ── Entry point ───────────────────────────────────────────────────────────────

/// Infer the type of an interpolated string `$"…"`.
///
/// Each `${expr}` hole is inferred; the hole's resolved type is checked for a
/// `ToText` instance using `to_text_set`. When `to_text_set` is `None` (unit
/// tests that do not run the full pipeline), the check falls back to the
/// built-in closed set so that existing low-level tests remain green.
///
/// Literal text segments (`InterpPart::Text`) require no inference.
///
/// The result type is always `Text` regardless of errors in the holes.
#[expect(
    clippy::implicit_hasher,
    reason = "FxHashSet is the canonical hasher for this crate; matches the pattern in collect.rs and ctx.rs"
)]
pub fn infer_interp(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    parts: &[InterpPart],
    _span: Span,
    to_text_set: Option<&FxHashSet<TyConId>>,
) -> Type {
    for part in parts {
        match part {
            // Literal text segment — no inference needed.
            InterpPart::Text { .. } => {}

            // Expression hole `${expr}`.
            InterpPart::Expr { expr, span } => {
                let t = infer_expr(ctx, b, expr);
                // Use deep_resolve to follow the full union-find chain. This
                // handles cases where the hole type was unified transitively
                // (e.g. via HOF parameter unification).
                let tr = ctx.deep_resolve(&t);
                check_hole_to_text(ctx, b, &tr, *span, to_text_set);
            }
        }
    }

    // The interpolated string always produces a Text value.
    Type::Con(b.text, vec![])
}

/// Check that a resolved hole type has a `ToText` instance, emitting T029 if
/// not.
///
/// When `to_text_set` is `None` (unit-test scaffolding without the full
/// pipeline), the built-in closed set is used as a fallback to keep existing
/// tests green.
fn check_hole_to_text(
    ctx: &mut InferCtx,
    b: &BuiltinTyCons,
    tr: &Type,
    span: Span,
    to_text_set: Option<&FxHashSet<TyConId>>,
) {
    match tr {
        // Type::Error or free type variable — absorb silently. Error types have
        // already been covered by an earlier diagnostic; free variables are
        // deferred to the constraint solver which verifies the instance at every
        // concrete call site.
        Type::Error | Type::Var(_) => {}

        // Concrete type constructor: consult the instance set.
        Type::Con(tycon_id, _) => {
            let has_instance = to_text_set.map_or_else(
                || builtin_has_to_text(b, *tycon_id),
                |set| set.contains(tycon_id),
            );

            if !has_instance {
                let ty_name = format!("{tr}");
                ctx.errors.push(TypeError::NoInstance {
                    class: "ToText".to_string(),
                    ty: ty_name,
                    span,
                    fix_hint: "add `instance ToText T` or `deriving (ToText)` to the type"
                        .to_string(),
                });
            }
        }

        // Any other type form (Fn, Tuple, Alias, etc.) — no ToText instance.
        other => {
            let ty_name = format!("{other}");
            ctx.errors.push(TypeError::NoInstance {
                class: "ToText".to_string(),
                ty: ty_name,
                span,
                fix_hint: "add `instance ToText T` or `deriving (ToText)` to the type".to_string(),
            });
        }
    }
}

/// Returns `true` when `tycon_id` belongs to the built-in closed set of types
/// that always have a `ToText` instance in the prelude.
///
/// Used as a fallback when the instance registry is not available (unit-test
/// scaffolding without the full pipeline). The prelude always registers these
/// instances so any context with a real registry will agree.
#[must_use]
fn builtin_has_to_text(b: &BuiltinTyCons, tycon_id: TyConId) -> bool {
    tycon_id == b.int
        || tycon_id == b.float
        || tycon_id == b.bool
        || tycon_id == b.text
        || tycon_id == b.timestamp
        || tycon_id == TyConId(15) // Ordering
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ridge_ast::{Expr, Ident, InterpPart, Span};
    use ridge_types::{TyConArena, TyConDecl, TyConId, TyConKind};

    fn ds() -> Span {
        Span::point(0)
    }

    fn make_builtins() -> BuiltinTyCons {
        let mut arena = TyConArena::new();
        BuiltinTyCons::allocate(&mut arena)
    }

    /// Build an `Expr::Interp` with a single expression hole containing `expr`.
    fn single_hole(expr: Expr) -> Expr {
        Expr::Interp {
            parts: vec![InterpPart::Expr {
                expr: Box::new(expr),
                span: ds(),
            }],
            span: ds(),
        }
    }

    /// Build an `Expr::Ident` bound to a variable already in scope.
    fn ident(name: &str) -> Expr {
        Expr::Ident(Ident {
            text: name.to_string(),
            span: ds(),
        })
    }

    /// Push a variable binding of type `ty` into `ctx.env`.
    fn bind(ctx: &mut InferCtx, name: &str, ty: Type) {
        use ridge_types::Scheme;
        ctx.env.push_frame();
        ctx.env.bind(name.to_string(), Scheme::mono(ty));
    }

    // ─── T1: Int hole ────────────────────────────────────────────────────────

    /// Test 1 — `"x = ${x}"` where x : Int → Text, no errors.
    #[test]
    fn interp_int_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "x", Type::Con(b.int, vec![]));

        let parts = vec![
            InterpPart::Text {
                raw: "x = ".to_string(),
                span: ds(),
            },
            InterpPart::Expr {
                expr: Box::new(ident("x")),
                span: ds(),
            },
        ];
        // Pass None for instance_env: the built-in closed-set fallback applies.
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Int hole; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.text),
            "expected Text result; got {ty:?}"
        );
    }

    // ─── T2: Float hole ──────────────────────────────────────────────────────

    /// Test 2 — `"y = ${y}"` where y : Float → Text, no errors.
    #[test]
    fn interp_float_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "y", Type::Con(b.float, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("y")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Float hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T3: Bool hole ───────────────────────────────────────────────────────

    /// Test 3 — `"flag = ${flag}"` where flag : Bool → Text, no errors.
    #[test]
    fn interp_bool_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "flag", Type::Con(b.bool, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("flag")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Bool hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T4: Text hole ───────────────────────────────────────────────────────

    /// Test 4 — `"name = ${name}"` where name : Text → Text, no errors.
    #[test]
    fn interp_text_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "name", Type::Con(b.text, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("name")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Text hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T5: Timestamp hole ──────────────────────────────────────────────────

    /// Test 5 — `"at = ${ts}"` where ts : Timestamp → Text, no errors.
    #[test]
    fn interp_timestamp_ok() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "ts", Type::Con(b.timestamp, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("ts")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for Timestamp hole; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T6: User record without a ToText instance → T029 ────────────────────

    /// Test 6 — `"u = ${user}"` where `user : User { name: Text, age: Int }` and
    /// no `ToText User` instance is registered → T029 `NoInstance`.
    ///
    /// The registry-path emits T029; the closed-set fallback (no registry) also
    /// produces an error because a non-builtin `TyConId` is not in the fallback
    /// set.  Both paths are covered here.
    #[test]
    fn interp_user_record_no_instance_t029() {
        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        // Intern a minimal User record type.
        let user_id = arena.intern(TyConDecl {
            id: TyConId(0), // overwritten by intern
            name: "User".to_string(),
            arity: 0,
            kind: TyConKind::Record(ridge_types::RecordSchema::new(
                vec![],
                vec![
                    ridge_types::RecordField {
                        name: "name".to_string(),
                        ty: Type::Con(b.text, vec![]),
                    },
                    ridge_types::RecordField {
                        name: "age".to_string(),
                        ty: Type::Con(b.int, vec![]),
                    },
                ],
            )),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        let mut ctx = InferCtx::new();
        bind(&mut ctx, "user", Type::Con(user_id, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("user")),
            span: ds(),
        }];
        // No registry → closed-set fallback: user_id is not a builtin → T029.
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected exactly one T029 for user type with no ToText; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(&ctx.errors[0], TypeError::NoInstance { class, .. } if class == "ToText"),
            "expected T029 NoInstance(ToText); got {:?}",
            ctx.errors[0]
        );
        assert_eq!(ctx.errors[0].code(), "T029");
        // Result is still Text (errors don't change the return type).
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T6b: User record WITH a registered ToText instance → no error ────────

    /// Test 6b — with a `ToText User` instance registered, the hole is accepted.
    #[test]
    fn interp_user_record_with_instance_ok() {
        use rustc_hash::FxHashSet;

        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);

        let user_id = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "User".to_string(),
            arity: 0,
            kind: TyConKind::Record(ridge_types::RecordSchema::new(vec![], vec![])),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        // Build a ToText set that includes user_id.
        let mut set: FxHashSet<TyConId> = FxHashSet::default();
        set.insert(user_id);

        let mut ctx = InferCtx::new();
        bind(&mut ctx, "user", Type::Con(user_id, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("user")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), Some(&set));

        assert!(
            ctx.errors.is_empty(),
            "expected no errors when ToText instance exists; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T7: List without instance → T029 ────────────────────────────────────

    /// Test 7 — `"l = ${l}"` where l : List Int and no `ToText` List instance
    /// is registered → T029 (not T012).
    #[test]
    fn interp_list_no_instance_t029() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        // List Int = Con(list, [Con(int, [])])
        bind(
            &mut ctx,
            "l",
            Type::Con(b.list, vec![Type::Con(b.int, vec![])]),
        );

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("l")),
            span: ds(),
        }];
        // No registry: List is not in the built-in closed set → T029.
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert_eq!(
            ctx.errors.len(),
            1,
            "expected exactly one T029 for List; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(&ctx.errors[0], TypeError::NoInstance { class, .. } if class == "ToText"),
            "expected T029 NoInstance(ToText); got {:?}",
            ctx.errors[0]
        );
        assert_eq!(ctx.errors[0].code(), "T029");
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T8: Type::Error absorbing — no error stacked ────────────────────────

    /// Test 8 — when the hole expression already resolves to `Type::Error`,
    /// no `ToText` error must be emitted (absorbing rule).
    #[test]
    fn interp_error_absorbing() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        // Simulate a hole that types as Error (e.g., an unknown identifier).
        // `Expr::Ident` for an unbound name → T001 + Type::Error.
        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("unknown_var_xyz")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        // There should be exactly one error (T001 from the unbound ident),
        // and it must NOT be a T029 NoInstance.
        let no_instance_count = ctx
            .errors
            .iter()
            .filter(|e| matches!(e, TypeError::NoInstance { .. }))
            .count();
        assert_eq!(
            no_instance_count, 0,
            "no ToText error must fire when sub-expr is Type::Error; errors: {:?}",
            ctx.errors
        );
        // Result is still Text.
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T9: No-hole interp — literal text only, types as Text ───────────────

    /// Test 9 — `$"hello world"` with only a `Text` segment (no `${…}` holes)
    /// types as `Text` with no errors.
    #[test]
    fn interp_no_holes_just_text() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();

        let parts = vec![InterpPart::Text {
            raw: "hello world".to_string(),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T10: Multiple holes, both in closed set ──────────────────────────────

    /// Test 10 — `"a=${a}, b=${b}"` where a : Int, b : Float → Text, no errors.
    #[test]
    fn interp_multiple_parts_ok() {
        use ridge_types::Scheme;
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        ctx.env.push_frame();
        ctx.env
            .bind("a".to_string(), Scheme::mono(Type::Con(b.int, vec![])));
        ctx.env.bind(
            "b_var".to_string(),
            Scheme::mono(Type::Con(b.float, vec![])),
        );

        let parts = vec![
            InterpPart::Text {
                raw: "a=".to_string(),
                span: ds(),
            },
            InterpPart::Expr {
                expr: Box::new(ident("a")),
                span: ds(),
            },
            InterpPart::Text {
                raw: ", b=".to_string(),
                span: ds(),
            },
            InterpPart::Expr {
                expr: Box::new(ident("b_var")),
                span: ds(),
            },
        ];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), None);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors for multi-part interp; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T11: infer_interp via infer_expr dispatch ────────────────────────────

    /// Test 11 — verify that `infer_expr` dispatches into `infer_interp`
    /// correctly: an `Expr::Interp` with a single Int hole types as Text.
    #[test]
    fn infer_expr_dispatches_interp() {
        let b = make_builtins();
        let mut ctx = InferCtx::new();
        bind(&mut ctx, "n", Type::Con(b.int, vec![]));

        let expr = single_hole(ident("n"));
        let ty = infer_expr(&mut ctx, &b, &expr);

        assert!(
            ctx.errors.is_empty(),
            "expected no errors from infer_expr dispatch; got {:?}",
            ctx.errors
        );
        assert!(
            matches!(ty, Type::Con(id, _) if id == b.text),
            "expected Text; got {ty:?}"
        );
        // Critically, no T999 InternalTypeError should be present.
        let t999_count = ctx
            .errors
            .iter()
            .filter(|e| matches!(e, TypeError::InternalTypeError { .. }))
            .count();
        assert_eq!(t999_count, 0, "T999 must not fire after T11 wiring");
    }

    // ─── T12: ToText set with builtins — builtins still accepted ─────────────

    /// Test 12 — when a `ToText` set built from the prelude registry is provided,
    /// builtins are accepted through the O(1) set membership check.
    #[test]
    fn interp_builtins_accepted_via_registry() {
        use crate::class_env::{register_prelude_instances, InstanceEnv};
        use ridge_types::TOTEXT_CLASS;
        use rustc_hash::FxHashSet;

        let b = make_builtins();
        let mut env = InstanceEnv::new();
        register_prelude_instances(&mut env);

        // Build the ToText set from the prelude env.
        let set: FxHashSet<TyConId> = env
            .instances
            .keys()
            .filter_map(|(class, head)| {
                if *class == TOTEXT_CLASS {
                    head.first().copied()
                } else {
                    None
                }
            })
            .collect();

        let mut ctx = InferCtx::new();
        bind(&mut ctx, "n", Type::Con(b.int, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("n")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), Some(&set));

        assert!(
            ctx.errors.is_empty(),
            "builtins must be accepted via the ToText set; got {:?}",
            ctx.errors
        );
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }

    // ─── T13: missing ToText instance with full set → T029 ───────────────────

    /// Test 13 — with a full prelude `ToText` set, a user type that has no
    /// registered `ToText` instance yields T029 (not T012).
    #[test]
    fn interp_missing_instance_with_registry_t029() {
        use crate::class_env::{register_prelude_instances, InstanceEnv};
        use ridge_types::TOTEXT_CLASS;
        use rustc_hash::FxHashSet;

        let mut arena = TyConArena::new();
        let b = BuiltinTyCons::allocate(&mut arena);
        let user_id = arena.intern(TyConDecl {
            id: TyConId(0),
            name: "Widget".to_string(),
            arity: 0,
            kind: TyConKind::Record(ridge_types::RecordSchema::new(vec![], vec![])),
            def_span: None,
            def_module_raw: None,
            opaque: false,
            is_anon: false,
        });

        let mut env = InstanceEnv::new();
        register_prelude_instances(&mut env);
        // Widget has no ToText instance in the registry.

        let set: FxHashSet<TyConId> = env
            .instances
            .keys()
            .filter_map(|(class, head)| {
                if *class == TOTEXT_CLASS {
                    head.first().copied()
                } else {
                    None
                }
            })
            .collect();

        let mut ctx = InferCtx::new();
        bind(&mut ctx, "w", Type::Con(user_id, vec![]));

        let parts = vec![InterpPart::Expr {
            expr: Box::new(ident("w")),
            span: ds(),
        }];
        let ty = infer_interp(&mut ctx, &b, &parts, ds(), Some(&set));

        assert_eq!(ctx.errors.len(), 1, "expected T029; got {:?}", ctx.errors);
        assert_eq!(ctx.errors[0].code(), "T029");
        assert!(matches!(ty, Type::Con(id, _) if id == b.text));
    }
}
